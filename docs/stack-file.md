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
requires  = "0.20.0"            # oldest devstack that can run this file
reviewed  = "0.20.0"            # devstack this file was last brought up to date with
env_files = [".env.defaults", ".env"]   # for ${VAR} everywhere; missing files are skipped
```

`name` must not be empty. `requires` is a version like `0.11.0`, and is
checked before anything else: an older devstack asks to be
updated instead of failing on a key it does not know. Raise it when the file
starts using something new. A pre-release devstack (`0.9.8-rc1`) counts as
older than its release (`0.9.8`). Next comes `version`: a file of another
schema version says so, not which of its keys is unknown.

`reviewed` names the devstack the file was last brought up to date with:
what changed since read, the file held against this reference. While it is
older than the devstack installed, or not set, the dashboard offers a
coding agent the upgrade, whose prompt sets it at the end; `devstack init`
writes it. It is not the agent skills' version: refreshing them (`devstack
skills install`) does not bring the file up to date. A devstack before
0.20.0 does not know the key: `devstack check` warns while `requires` is
lower than that.

### Names, paths, env files

- **Names** of services, Compose groups, commands and groups use lowercase
  letters, digits and `-`, do not start with `-`, and are none of the names
  Windows reserves for devices (`con`, `prn`, `aux`, `nul`, `com1`-`com9`,
  `lpt1`-`lpt9`): they become directory names.
- **Paths** in `env_files`, `[compose.<group>] files` and `env_files`,
  `[lanes] dockerfile` and `sync`, and a service's `cwd`, `output`,
  `prepare.cwd` and `hot_reload.cwd` are relative to the repository and stay
  inside it: no absolute path, no drive (`C:`), no `..`, judged the same on
  every platform. `output` may start with `{repo}`; where `${VAR}` makes it
  absolute, the build fails unless it is in the snapshot.
- **Env files** are read in Compose's dialect: `KEY=VALUE` lines, `#`
  comments, an optional `export `; an unquoted value ends at ` #`; a
  `"double-quoted"` one may span lines and knows the escapes `\n`, `\r`,
  `\t`, `\\`, `\"`, `\$`, and `$$` for a `$`; a `'single-quoted'` one is
  taken as written. `${VAR}`, `${VAR:-default}`, `${VAR-default}` and `$VAR`
  in an unquoted or double-quoted value come from the process environment,
  then from what the files set before; an unknown one is empty. A default
  may hold another reference (`${A:-${B}}`); `${VAR:+alt}` and
  `${VAR+alt}` give `alt` when the variable is set (`:+`: and not empty).
  Unlike Compose, `${VAR:?message}` and `${VAR?message}` do not fail: they
  read as `${VAR}`. A byte order mark at the start of the file is skipped.
  (The stack file's own `${VAR}` knows `${VAR}` and `${VAR:-default}`
  only; see "Placeholders".)

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
main_ports = ["web"]                                     # their fixed ports move with `promote --lane` (below)
announced_ports = ["media:7881", "media:7882/udp"]       # ports the server tells its clients (below)
```

`main_ports` names the services whose published TCP ports devstack holds in
the main tree ([ADR-0048](adr/0048-a-containers-main-port-can-move-with-the-main-slot.md)):
the main tree's container is published on a loopback port Docker assigns, and
devstack listens on the fixed port the Compose files name (on their address:
every interface without one) and forwards it there. `devstack promote --lane
<name>` points it at the lane's container (the fixed port plus the lane's
offset), `--lane main` back; the main tree's container keeps running, and the
main tree's services still reach it. Name what you reach directly from the
host (the UI, the API the browser calls), not what those use. The port answers
only while the stack's daemon runs. UDP ports stay Docker's and do not move
([#192](https://github.com/nuetzliches/devstack/issues/192)), and so do
`announced_ports`: a client learns those from the server it signalled with,
so it reaches that tree's server anyway.

`announced_ports` names ports whose number the server tells its clients
([ADR-0057](adr/0057-a-port-a-container-announces-moves-inside-it-too.md)): a WebRTC media server's ICE
candidates, a broker's advertised listeners, an "external port" setting.
`"media:7881"` is TCP, `"media:7882/udp"` UDP. Each is published 1:1 on a
fixed port in the Compose files (`7881:7881`). A lane moves such a port
inside the container too (`7981:7981` in the first lane), so the server
listens on, and announces, the lane's number. Every `docker compose` call for
the group, and every `exec` step of a command, gets
`DEVSTACK_PORT_<SERVICE>_<PORT>` (the service upper-cased, `-` as `_`; the
port as the files have it; TCP and UDP of one number share it) with the
number clients reach in this tree: the files' port in the main tree, moved by
the lane's offset in a lane (a `shared` service's stays the main tree's). It
wins over an env file's value. Pass it to the server's listen and announce
settings, with the files' port as default so the files run without devstack;
`ports` stays literal, devstack moves it:

```yaml
services:
  media:
    ports: ["7881:7881", "7882:7882/udp"]
    environment:
      RTC_TCP_PORT: ${DEVSTACK_PORT_MEDIA_7881:-7881}
      RTC_UDP_PORT: ${DEVSTACK_PORT_MEDIA_7882:-7882}
```

devstack keeps calling the port by the files' number:
`{compose.app.media.7881.port}` is `7981` in the first lane, and the
dashboard shows `7881->127.0.0.1:7981`. Inside the lane's network the server
listens on the moved number: another container, or a health check, that
addresses `media:7881` uses the variable too (`media:${DEVSTACK_PORT_MEDIA_7881:-7881}`,
which a native service's `rewrite_hosts` rewrites as well). A `docker
compose` run by hand without the variable recreates the container on the
files' port. Port ranges cannot be announced. `devstack check` warns about a
`DEVSTACK_PORT_*` the group's files or env files use that no entry defines
(a typo, or the entry missing: the lane would announce the main tree's
port), one read without a default, and an entry whose variable no file
uses. A file with `announced_ports` should say `requires = "0.20.0"` in
`[stack]`.

Every `docker compose` call for a group, and every `exec` step of a command,
also gets the tree's lane
([ADR-0064](adr/0064-every-compose-call-is-told-the-lane-it-runs-for.md)):
`DEVSTACK_LANE` (the lane's name, `main` in the main tree) and
`DEVSTACK_LANE_SUFFIX` (empty in the main tree, `_<lane>` in a lane), the
values `{lane}` and `{lane_suffix}` have for a native service. They win over
an env file's value. A lane on a node is told its name as its developer
calls it, and so are its native services (`{lane}`, `{lane_suffix}`,
`DEVSTACK_LANE`): the same values as a local lane of that name. Read them
with a default, so the files give the main tree's value
without devstack; what a browser keeps per name, such as a login cookie,
stays apart per tree that way:

```yaml
services:
  scheduler:
    environment:
      REFRESH_COOKIE: app_refresh${DEVSTACK_LANE_SUFFIX:-}
```

`devstack check` warns about one read without a default, and about a
`DEVSTACK_LANE_*` devstack does not set (a typo). They take no key in the
stack file; a devstack before 0.23.0 does not set them, and the files then
give the main tree's value in every tree.

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
- `shared`, `main_ports`, `restart_on` and `urls` name services of
  `services`; `main_ports` none of `shared`; `urls`
  are `http://` or `https://` addresses or `""`; `restart_on` holds globs.
- `announced_ports` entries are `service:port` or `service:port/udp` (no
  range), name services of `services`, each once, and no two give one
  variable name. Each is published 1:1 on a fixed port in the Compose files:
  `devstack check --compose` checks that on the model, as every start does.
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
- `health` needs `port`; `proxy = true` only for a `service`, `static` or
  `devserver` with a `port` (a `devserver`'s `run` then takes `{port}`).
- A service's `timeout` is more than 0.
- `after` never names the service itself (nor a Compose group itself).
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
a file whose name carries its content hash (`index-Bd3x9_kQ.js`,
`main.3f2a1b9c.css`) may be cached for a year, every other file
(`index.html`, `config.json`, a manifest) is sent with `no-cache` and an
`ETag`, so a promoted build shows at the next load. The API the bundle
calls must be reachable from the browser as the bundle expects it (an
absolute URL, or the same origin through your own proxy setup).

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
- the `build` command and `output` as the build runs them (a `${VAR}` in
  them with its value, `{repo}` as written, so lanes share the id), `cwd`
  and `prepare` (`run`, `cwd`, `inputs`, and the content of the files
  `inputs` matches).

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
  pick up (`tsconfig.json`, `angular.json`), debounced 2 s like `watch`. A
  pattern that names a folder (`src/api`) covers what it holds, as
  `src/api/**` does; the same goes for a Compose service's `restart_on`. The
  daemon log has a `restart_on` line for each restart, with the paths that
  caused it and when the first of them changed (`restart_on_skipped` when the
  dev server was not running). A change while the dev server starts or
  restarts restarts it once more when it is up.
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
  hot-reload mode is read the same way (`auto`). `rebuild` belongs to
  `kind = "devserver"` only; an unknown tool name or a pattern that is no
  regular expression is an error.
- `proxy = false`: no proxy; a restart happens in place on the fixed port (a
  short gap). Only for services the proxy cannot carry.
- A dev server whose `run` takes `{port}` runs on a port of its own, and
  devstack holds its fixed port and forwards it byte for byte (TLS,
  WebSockets and HMR pass unchanged;
  [ADR-0043](adr/0043-a-dev-servers-fixed-port-is-devstacks.md)): a restart
  keeps the address (new connections wait for the new instance), and
  `promote --lane` switches the forward. `proxy = false` makes it bind the
  fixed port itself, as does a `run` without `{port}`. The dev server prints
  its own port in its log; the address to open is the fixed one.

Generated directories never count as changes: `bin`, `obj`, `node_modules`,
`target`, `.git`, `.devstack`, the caches of frontend tools (`.angular`,
`.vite`, `.next`, `.nuxt`, `.turbo`, `.cache`, `.svelte-kit`,
`.parcel-cache`) and of Python (`__pycache__`, `.pytest_cache`,
`.mypy_cache`, `.ruff_cache`), even where `.gitignore` does not list them. The watcher
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
- `timeout`: how long a start may take until it counts as failed; more
  than 0.
- `insecure_tls` (default `false`): the service speaks HTTPS with a
  self-signed certificate. The service's URL (`{service.x.url}`, the
  dashboard link) then uses `https`.

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
  (its lane, `main` in the main working tree; in a lane on a node the
  lane's name, as `{lane}`), `DEVSTACK_BUILD_ID` (the
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
([plan §3.4a](plan.md)); only for `kind = "service"` with `build` (a dev
server reloads itself).

### Other keys

| Key | Meaning |
| --- | --- |
| `autostart = false` | started only on demand (`devstack start <service>`) |
| `lanes = false` | runs in the main working tree only, never in a lane: for what may run once per machine (a tray app, something holding a global lock); see `[lanes]` |
| `platforms = ["windows"]` | only on these platforms (`windows`, `linux`, `macos`); default `[]`: every platform |
| `ensure_dirs = ["{repo}/data/files"]` | created before start |
| `prepare = { run = "npm ci", inputs = ["web/package-lock.json"] }` | installs what a copy of the source lacks (see below) |
| `timeout = "10m"` | `kind = "job"` only: how long it may run before it counts as failed and is stopped; default 30 minutes. `devstack stop <job>` ends it at once |
| `stop = "docker rm -f api{lane_suffix}-{port}"` | ends what `run` started outside its process tree, a container ([ADR-0059](adr/0059-a-services-stop-command-ends-what-its-run-started.md), below); not for `kind = "static"` |

### A container as a service (`docker run`)

A service whose runtime needs Linux tools can run as a container and still
get `watch`, candidates and a lane's own port, which a Compose container
does not:

```toml
[services.ocr]
build = "docker build -t ocr:dev ocr"
run   = "docker run --rm --init --name ocr{lane_suffix}-{port} -p {port}:8000 ocr:dev"
stop  = "docker rm -f ocr{lane_suffix}-{port}"
port  = 5100
health = { http = "/health", timeout = "60s" }
watch = ["ocr/**"]
```

devstack owns the `docker` CLI's process tree, not the container, which
runs in Docker's VM ([ADR-0009](adr/0009-the-daemon-owns-every-process-tree.md)):

- **The graceful stop reaches the container** through the CLI, which
  passes it on: `CTRL_BREAK` on Windows arrives there as `SIGINT`,
  `SIGTERM` elsewhere as `SIGTERM`. The process in the container must end
  on it: a container's PID 1 ignores a signal it has no handler for, so
  use `--init` or an entrypoint that handles both. Otherwise the CLI is
  killed after 20 s and the container keeps running.
- **`--rm`** removes the container once it ended.
- **A name with `{lane_suffix}-{port}`** (`{port}` is the instance's own,
  internal behind the proxy) keeps a candidate apart from the main instance
  and a lane from the main tree. A fixed name, or a `docker rm -f` before
  `docker run` in `run`, would remove the main instance while a candidate
  starts.
- **`stop`** runs whenever an instance ends (a stop, a restart, the old
  instance a candidate replaced, a start that failed, one that ended on its
  own), before the graceful signal, with that instance's placeholders, at
  most 30 s; its output and a failure go into the instance's log, and the
  stop goes on regardless. After the daemon died, its next start runs the
  `stop` of every instance it had (with devstack's environment, not the
  service's). It covers what the signal does not: a process that ignores
  it, a CLI that ended by itself. In hot-reload mode it does not apply.

A file with `stop` should say `requires = "0.21.0"` in `[stack]`.

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
prepare = { run = "npm ci", inputs = ["web/package-lock.json"], link = ["web/node_modules"] }
```

`link` ([ADR-0044](adr/0044-a-lanes-prepared-directories-can-be-linked-into-its-worktree.md))
makes what `prepare` installed usable in a lane's worktree, where git runs its
hooks (a pre-commit hook with `npx eslint`): each directory, relative to the
repository, is linked from the worktree to the lane's copy (a junction on
Windows, a symbolic link elsewhere) once `prepare` ran. Only where the
worktree has nothing of that name: your own `npm ci` there wins. Git has to
ignore the link: write `node_modules` in `.gitignore`, not `node_modules/`,
which on Linux and macOS ignores directories only (the daemon log says
`prepare_not_linked` with why). `devstack lane stop` removes the links again;
**stop the lane before `git worktree remove`**: on Windows, depending on the
Git version, it follows the junction and empties the copy's directory (the
next start prepares it again), or leaves the worktree behind with the junction
in it.
The main tree makes no links. A lane on a node links its worktree to a
copy on this machine, with `outputs` (below).

`outputs` ([ADR-0062](adr/0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md))
names the directories `prepare` makes, relative to the repository, for the
store: after a `prepare` that succeeded they are kept per machine and
repository (`.devstack/store/` of the main tree; on a node per member and
repository), under a key of `run`, `cwd`, the files of `inputs`, the
operating system and architecture, and on a node the lane image. The next
copy with the same key gets them copied and does not run `prepare`; a second
lane with the same lockfile starts without `npm ci`. Name everything
`prepare` makes that the service needs: what it writes elsewhere is not in
the store. `version` is a command whose output, run in `prepare`'s `cwd`,
is part of the key too, so that a toolchain update makes a new entry
(`node --version` for native packages); it needs `outputs`. The key
covers `outputs` too: two services whose `prepare` has the same `run`, `cwd`,
inputs, `outputs` and `version` share one entry. With `outputs`,
each `link` is one of them or inside one: a lane on a node links its
worktree to a copy from this machine's store
([ADR-0063](adr/0063-a-lane-on-a-node-links-its-worktree-to-a-copy-from-the-local-store.md)).

```toml
prepare = { run = "npm ci", inputs = ["web/package-lock.json"], outputs = ["web/node_modules"], link = ["web/node_modules"], version = "node --version" }
```

A hit copies many files at once (`prepare-*.log` says `prepare: from
store`, with the time); measured on Windows, a `node_modules` of 24,500 files
took 23 s to `npm ci` on NTFS and 5 s to copy, 45 s and 2 to 3 s on a Dev
Drive. Where a copy of an entry once took longer than the `prepare` that
made it, `prepare` runs instead; an output that holds files git tracks
keeps `prepare` out of the store (a notice says so). A second copy that needs a key another one
is preparing waits for it and copies what it made. Entries no copy used for
14 days go by themselves; `devstack lane prune --older-than <age>` removes
them sooner. A copy never shares a file with the store: what a dev server
writes into its `node_modules` stays in its copy.

**A lane on a node** ([ADR-0063](adr/0063-a-lane-on-a-node-links-its-worktree-to-a-copy-from-the-local-store.md))
runs `prepare` there, and its worktree here still gets `link`: devstack
checks the worktree's inputs every 15 seconds and, when their key is new,
copies the entry of this machine's store into `.devstack/lane-<name>/linked/`
or, without one, runs `prepare` there once (with a copy of the worktree's
source) and keeps its outputs in the store; then it links the worktree
there, by the rules above. The worktree's hooks get what is built for this
machine (Windows here, Linux on the node). `devstack lane list`, `devstack
status` and the dashboard say per path `linked`, `linked (store)`,
`preparing`, `linked (stale)` or `not linked` with why. New inputs are
prepared in a second copy while the worktree keeps the one it has; the link
moves once that copy is whole, and stays where it was when its `prepare`
fails (`linked (stale)`, with why). A linked directory that lost most of
what `prepare` made (a reinstall in the worktree goes through the link into
the lane's copy) is prepared again: for a lane here when its service next
starts, with a notice, for a lane on a node at the next look. Without `outputs` it is not linked, and
says so. The links go when the lane stops on its node, moves or goes; the
copy stays for its next start.

**On a node** the member has a store of its own per repository: the lane's
container reads it (mounted read-only), and what a lane prepares there goes
into it through the lane's slot, taken in by the node within seconds, so
the next lane of the member with the same inputs, platform and lane image
copies it there instead of running `prepare`. The node keeps entries no
lane used for as long as it keeps a stopped lane, counts them with the
member's kept lanes, and lets them go first when the member is over its
room ([pool.md](pool.md)). A node of an older devstack has no store: its
lanes prepare in every copy.

A file with `outputs` or `version` should say `requires = "0.23.0"` in
`[stack]`.

### `[prepare] warm`: the store made ahead for a ref

```toml
[prepare]
warm = ["origin/main"]
```

([ADR-0065](adr/0065-the-store-is-warmed-for-a-ref-before-a-lane-needs-it.md))
The first lane after a lockfile change pays the whole `prepare`; with
`warm`, the main tree's daemon pays it ahead, in the background. It reads
each ref's commit every minute and never fetches: whoever fetches decides
when the ref moves. A new commit, unchanged for two minutes, is checked
out under `.devstack/warm/` as git checks it out (line endings and filters,
so the key is a checkout's), with an index of its own: your worktree, your
index and the list of worktrees stay as they are. First only each
service's `inputs`: when the store has their key, that is all. Otherwise
the whole ref is checked out there and `prepare` runs once, under the same
one-at-a-time lock as every `prepare` (a lane that needs the key meanwhile
waits and copies), and its `outputs` go into the store. Then the nodes this
repository's lanes ran on, while one of them is kept or runs there, get a
**warm lease**: a short lease of yours that runs only the `prepare` their
store lacks, without containers, and ends ([pool.md](pool.md)). A commit is
warmed once (also over a restart); a failure is a notice, retried at the
next commit. Each service whose `prepare` names `outputs` is warmed; a
`warm` without such a service is an error. A file with `warm` should say
`requires = "0.24.0"` in `[stack]`.

## `[lanes]`: other worktrees ([ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md); on other machines [ADR-0025](adr/0025-a-lane-can-run-on-another-node.md), [ADR-0028](adr/0028-lane-images.md), [ADR-0029](adr/0029-source-sync-and-port-forwarding-for-remote-lanes.md), [ADR-0035](adr/0035-lanes-can-be-shared-with-the-pool.md), and [pool.md](pool.md))

```toml
[lanes]
port_step = 100   # the default: lane 1 runs api 5000 on 5100, lane 2 on 5200
# For lanes on another machine of the pool (ADR-0025, ADR-0028), one of:
image      = "registry.example/shop-lane:1"   # a Linux image with the stack's toolchains
dockerfile = "dev/lane.Dockerfile"            # or one the node builds from the lane's source
sync       = ["web/certs/*.pem"]              # ignored files such a lane needs all the same
share      = ["team:qa"]                      # whom such a lane is shared with by default (`*`: everyone); none: nobody
keep_data  = true                             # the default: such a lane keeps its data on its node for its next start there (ADR-0038)

[lanes.need]                                  # what a lane needs at least (ADR-0049): a floor, never an override
cpus   = 2
memory = "6G"                                 # sizes: 512M, 6G, 25GiB (powers of 1024); a bare number is bytes
disk   = "25G"                                # in all: images, source, dependencies, builds, volumes
gpus   = { count = 1, memory = "16G" }        # a lane in the pool goes only to a node that offers them
```

`port_step` is 1-10000. `dockerfile` and `sync` stay inside the repository
(see "Names, paths, env files"); `sync` holds globs; `image`, if set, is
not empty. Two services whose ports differ by a multiple of `port_step`
(`api = 5000`, `web = 5100`, step 100) collide in a lane: the daemon skips
those lane numbers, and `devstack check` warns where that hits one of the
first three lanes.

Every `git worktree` of the repository is a **lane** of the main tree's
daemon, the moment git creates it; the dashboard shows it under the stack,
named by its branch. `devstack up` in it starts it: its own build, its own
ports (moved by `port_step` times the lane's number), **its own containers**
([ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md)), no background
workers (`devstack up --workers` turns them on). Every devstack command and the
MCP server in the worktree act on its lane.

A lane runs with its worktree's own stack file, at the main one's path
(`devstack.toml`, or where `-f` names it below the repository). A branch
without one (older than devstack in the repository, or the file was never
committed) runs with the main working tree's, on a node of the pool too
([ADR-0055](adr/0055-a-lane-without-a-stack-file-says-so-and-can-be-given-one.md)).
The dashboard says so: the branch picker marks such a branch, and the lane's
page offers to copy the main tree's file into the worktree (to change for the
branch and commit with it), or a prompt for a coding agent that adapts it.

Worktrees are git's (or your tool's): devstack never creates or removes one,
and nothing of a lane runs inside its worktree. Builds run in the lane's
snapshot, and so do its dev servers, tools and hot-reload mode; the snapshot
follows the worktree within moments of a change (hot module replacement keeps
working), and `{repo}` is the snapshot for them. So `git worktree remove`
works while the lane runs, and the lane stops with it (with `prepare.link`,
stop the lane first: see `prepare`). Ignored files are not in the snapshot:
`prepare` installs what is missing.

- A lane's containers are a Compose project of their own,
  `<project>-<lane>`, from its worktree's Compose files, with an overlay
  devstack writes (`.devstack/lane-<name>/compose/<group>.yml`): host ports
  moved by the lane's offset (an `announced_ports` port inside the container
  too), no fixed `container_name`, fixed-name networks
  and volumes renamed `<name>-<lane>`. Its first start migrates and seeds its
  own data. `lane stop` removes its containers and keeps their volumes; a
  removed worktree takes them all. `{compose.<group>.project}` is the
  project as the tree runs it: a command's `docker compose -p` with it
  reaches the lane's containers in a lane (see `[commands]`).
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
- `need` ([ADR-0049](adr/0049-a-lanes-need-is-measured-disk-included.md)):
  what a lane needs **at least**. The pool places a lane by what the
  repository's lanes took where they ran (the most of each part over the
  last five runs, measured by the nodes), or, before any ran there, by what
  this machine estimates from the main tree; `need` raises either to what it
  says, it never lowers them. Every key is optional; `cpus` is above 0, a
  `gpus` table asks for `count` GPUs (1 if not given) with at least
  `memory` each. A lane on this machine checks its disk against what the
  main tree's snapshot, artifacts, logs and volumes take, not against
  `need`: it shares the images, and its worktree is there already. A file
  with `need` should say `requires = "0.17.0"` in `[stack]`.
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
  the main tree's instance serves every lane. A command's `start`, `stop` or
  `restart` step on it is skipped in a lane, with a line in the run's log,
  so a command written for both trees does not fail there. It keeps its
  background workers while a lane holds the main slot, and `promote --lane`
  leaves it alone.
- A dev server runs in a lane only if it takes `{port}`, in its `run` or in
  its `env` (a Vite or Angular config that reads its port from a variable):
  otherwise its fixed port is the main tree's. In `env` only, it binds the
  lane's port itself (as with `proxy = false`); a `run` with `{port}` gets
  devstack's forward (ADR-0043).
- A dev server in a lane runs from the lane's copy, without the ignored
  files: one that needs `node_modules` needs `prepare` (`devstack check`
  warns about a dev server whose `cwd` has a `package.json` and no
  `prepare`). Without `prepare`, one that ends in a lane before it is
  ready says in its error that the copy lacks the ignored files.
- The main tree and every lane are on `localhost`, and browsers do not tell
  cookies apart by port: a login in a lane's frontend replaces the main
  tree's in the same browser. A cookie name with `{lane_suffix}` (in the
  service's `env`) keeps them apart; in a Compose service, with
  `${DEVSTACK_LANE_SUFFIX:-}` in its `environment` (above). Both hold for a
  lane on a node too, whose ports are on your `localhost` as well.
- `{service.<name>.url}` is `http://127.0.0.1:<port>`, while the dashboard
  links `http://localhost:<port>`: an Origin check (CORS, CSRF) built from
  the URL refuses a browser that came through the dashboard's link. Build
  the allowed origins from `{service.<name>.port}`, with both hosts.
- `devstack promote --lane <name>` lets a lane's services answer on the main
  ports (dev servers through a TCP forward); `--lane main` gives them back.
  The main ports are the fixed ports of `[services.*]` and of the
  containers `[compose.<group>] main_ports` names, but for their
  `announced_ports`; any other container's
  published port is Docker's and stays with the main tree's container, so a
  stack with nothing of either has nothing to move, and `promote --lane`
  refuses it.
- `devstack lane list`; `devstack lane start <name>` starts a lane from
  anywhere in the repository, as `devstack up` in its worktree does;
  `devstack lane stop <name>` stops a lane (its builds stay, the worktree
  too); `devstack lane reset <name>` throws a stopped lane's data away (its
  containers and volumes, here and on the nodes it ran on), so it starts
  fresh; `devstack lane prune [--older-than 7d] [--dry-run]` removes what
  lanes whose worktree is gone left behind, here and on the pool's nodes
  (ADR-0038); with `--older-than`, also entries of the store of `prepare`
  that no copy used for as long (ADR-0062).

## `[commands.<name>]`: operations

```toml
[commands.reset-db]
description = "Drop the database, migrate, seed"
confirm     = "The local database will be dropped. Continue?"   # nothing runs without a yes
requires    = ["api"]                                           # enabled only while these are healthy
steps = [
  { stop = "api" },
  { exec = "docker compose -p {compose.app.project} exec -T db dropdb -U dev shop" },
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
published address of a Compose service's port, as for services, and
`{compose.<group>.project}`) and `${VAR}`; `{port}`, `{artifact}` and
`{is_main}` have no value in a command.

Name a Compose project with `{compose.<group>.project}`, never literally:
the same command runs in a lane, whose containers are the project
`<project>-<lane>`. With `-p shop-dev` written out, the example above would
stop the lane's API and drop the **main tree's** database. `docker compose
-p <project> exec` finds a running project's containers without its files.
A file that uses `{compose.<group>.project}` should say `requires =
"0.20.0"` in `[stack]`. An `exec` step gets the `DEVSTACK_PORT_*` of every
group's `announced_ports` as the tree has them, so `docker compose -p
{compose.app.project} up -d media` in a lane keeps the moved port, and the
tree's `DEVSTACK_LANE` and `DEVSTACK_LANE_SUFFIX` (ADR-0064).
In a lane, a `start`, `stop` or `restart` step on a `lanes = false` service
is skipped (the main tree's instance serves the lane), with a line in the
run's log.

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
need      = { memory = "8G" }            # a lane with it needs at least this, over [lanes.need] (ADR-0049)

[profiles.acceptance.services.web]
kind   = "static"                        # a build served as files instead of a dev server
build  = "npm run build -- --configuration e2e"
output = "dist/web/browser"

[profiles.lite]
description = "daily work without the heavy containers"
skip      = ["compose.search", "compose.app.cache", "worker"]   # a group, a container, a service
```

```sh
devstack up --profile acceptance [--run]              # in a worktree: its lane
devstack lane start <lane> --profile acceptance --run  # from the main tree
```

- **What it may set:** `workers`, `data` (`keep`, the default, or
  `fresh`), `env` (for every service and every command's `exec` steps, with
  the placeholders a command has: no `{port}`, `{artifact}` or
  `{is_main}`), `autostart`, `skip` (below), `command` (one of `[commands]`), `need` (as
  `[lanes.need]`, each part the larger of the two; a lane with the profile
  is measured on its own, its runs are the profile's), and
  per service `kind`, `build`, `output`, `run`, `stop`, `spa`, `cwd`,
  `health` and `env`. A service it does not name runs as the file says. Its `env` is
  added to every service's, a service's own `env` in the profile on top.
  `watch`, `port` and the other keys stay as the file says.
- **`skip`** ([ADR-0058](adr/0058-a-profile-can-leave-out-containers-and-services.md))
  leaves out what the profile does not need: a container
  (`compose.<group>.<service>`, one of the group's `services`), every
  container of a group (`compose.<group>`), or a service (as `autostart =
  false`; a name in `autostart` too is an error). A skipped container does
  not start with the tree, and one an earlier start left running is
  stopped. It stays part of the stack: `devstack start
  compose.app.cache` starts it on demand, and a port `main_ports` holds
  for it stays held and refuses connections while it is down. A service
  that starts and needs a skipped container's address (a `{compose.…}`
  placeholder in its `run`, `env`, `workers.env` or `ensure_dirs`, the
  profile's `env` included, or a `rewrite_hosts` entry) is an error, since
  its start would fail: skip it too, or keep the container. One that only
  waits for it (`after`) starts without waiting. On a node a skipped
  container is not created, so it cannot be started there later in that
  run. A file with `skip` should say `requires = "0.21.0"` in `[stack]`.
- **Another `kind` drops what only the old one takes:** a dev server made
  `static` loses its `run` and `stop` (unless the profile gives them), `rebuild` and
  `restart_on`; a `devserver` or `tool` loses `build` and `output`; `spa`
  stays with `static` only, `hot_reload` with `service`, `timeout` with
  `job`; a `job` or `tool` loses `health` and `proxy`. What the profile sets
  itself comes on top.
- **Its name** uses letters, digits, `-` and `_`; `description` is optional,
  shown where a profile is chosen.
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
`label` defaults to the group's key; groups without `order` follow those
with one, alphabetically. `devstack start|stop|restart <group>` and the
group's buttons in the dashboard act on every member, its containers too
(a lane leaves the main tree's `shared` ones alone); `restart --rebuild`
builds its services again even for inputs built before.
`members` is required, not empty, lists each member once, and names what
exists (a `compose.<group>.<service>` member is a service its group starts).

## `[agents]`: what devstack writes for coding agents

```toml
[agents]
skills = [".claude/skills", ".agents/skills"]   # default: [".claude/skills"]
```

`skills` names the folders `devstack skills install` writes the generated
agent skills to, relative to the stack file. Claude Code reads
`.claude/skills` only, Codex `.agents/skills` only; Copilot, Cursor,
Gemini CLI, OpenCode, Amp and Windsurf read `.agents/skills`, most of them
`.claude/skills` too, and then find each skill twice: name both only when
the repository's agents need both. `devstack init` fills it in from what the
repository shows (`.claude/` or `CLAUDE.md`; `AGENTS.md`, `.agents/`,
`.codex/`, `.cursor/`, `.gemini/` and the like). Each folder must stay
inside the repository and be named once; the list must not be empty.

A generated skill (its marker line says so) is also looked for in the other
agents' folders (`.github/skills`, `.cursor/skills`, `.gemini/skills`, ...):
`devstack status`, `doctor`, `skills check` and the dashboard report one that
no longer matches (a newer devstack whose skills read the same, apart from
its version, does not count), and `skills install` refreshes it there. A skill the
project wrote itself is never touched.

## Placeholders

| Placeholder | Value |
| --- | --- |
| `{repo}` | absolute path of the lane's source root |
| `{artifact}` | artifact directory of the build being started |
| `{port}` | port of the instance being started (an internal one behind the proxy) |
| `{is_main}` | `true` in the main slot, `false` in a candidate and in a lane without the main slot |
| `{lane}` | the lane's name, `main` in the main working tree; in a lane on a node the lane's name as you call it, as on your machine |
| `{lane_suffix}` | empty in the main working tree, `_<lane>` in a lane (on a node too): `app{lane_suffix}` keeps the main tree's name |
| `{service.<name>.url}` | URL of that service's main slot (`http(s)://127.0.0.1:<port>`) |
| `{service.<name>.port}` | that service's port |
| `{compose.<group>.project}` | the group's Compose project as this tree runs it: `project` in the main working tree, `<project>-<lane>` in a lane, on a node the project the node runs for the lane (for `docker compose -p` in a command; a `shared` service stays in the main tree's) |
| `{compose.<group>.<service>.<port>}` | where a container port is published, as a URL (`http://127.0.0.1:15432`) |
| `{compose.<group>.<service>.<port>.host}`, `.port`, `.address` | its host (`127.0.0.1`), its port (`15432`), or both (`127.0.0.1:15432`), for connection strings: `Server={compose.app.sql.1433.host},{compose.app.sql.1433.port}`, `Host=…host;Port=…port`. In a lane: the lane's own container, or the main tree's for a `shared` one |
| `${VAR}`, `${VAR:-default}` | from the env files and the process environment; placeholders inside a default are resolved too |

`{{` and `}}` are literal braces, inside a `${VAR:-default}` too (where a
single `}` ends the default). Durations: `500ms`, `30s`, `5m`, `1h` (a bare
number is seconds).

Where they have a value: `{port}` only in a service with a `port`,
`{artifact}` only in a service with `build` (not in `hot_reload.run`),
`{service.<name>.…}` only for a service with a `port`, `{compose.…}` only
for a service its group starts (`{compose.<group>.project}`: for a group
the file has). `build` and `output` take `{repo}` and
`${VAR}` only (every lane shares the artifact); commands (their `exec`, an
`exec` step's `cwd` included) no `{port}`, `{artifact}` or `{is_main}`; a
service's `cwd`, `prepare.run`, `prepare.cwd` and `hot_reload.cwd` none at
all.
