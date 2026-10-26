# devstack: plan

- **Status:** Active: phases 0 to 2 done (0.4.0), phase 3 done (lanes per worktree 0.7.0, each with its own infrastructure since 0.8.0, ADR-0030), phase 4 under way; the pool since 0.9.0: lanes on other machines, moved, shared, given the main ports; node policies, several pools, capacity over time, telemetry with consent (ADR-0025 to ADR-0037, 0.9.0 to 0.9.8; [pool.md](pool.md), §7.4, backlog §13); 0.10.0 (2026-10-18) carries the fixes of the review of 0.9.8: lanes confined on a node, nodes granted by an admin, the promotion gate and restore of ADR-0013/0016 as written, the hub's origin rules; 0.11.0 (2026-10-19): lanes that outlive the main tree's stop, keep their data on a node, run with a profile, and can be viewed when shared; a server updated by an admin (ADR-0038 to ADR-0041); 0.12.0 (2026-10-20): sharing a lane includes viewing it (ADR-0040 amended), a remote lane's containers controlled through its node, a dev server's rebuilds shown like builds (ADR-0042), a node's own lines in the pool's telemetry; 0.13.0 (2026-10-20): a dev server's fixed port is devstack's, forwarded to its instance (ADR-0043); 0.14.0 (2026-10-21): the pool places a lane where it starts warm (ADR-0026 point 3), where a lane starts without `--on` is the machine's choice (ADR-0025 amended), a lane's start on a node shows its phases; 0.15.0 (2026-10-21) carries the fixes of a review of the whole code base: a node runs the Compose model it checked, several agents share one engine, the supervisor keeps edits and stops, stack file paths stay in the repository; 0.16.0 (2026-10-21): desktop notifications from the hub and lanes of other members shown (ADR-0045), winget on Windows, a stack's folder opened in one's own programs (ADR-0046), the share switch removed (ADR-0031 amended); 0.17.0 (2026-10-21): a lane made from a branch in the dashboard (ADR-0047), containers on the main ports (ADR-0048), a lane's need measured where it ran, disk included (ADR-0049); 0.18.0 (2026-10-21): notices for what no request reports (ADR-0050), a lane whose worktree is removed shown while it goes (ADR-0024 point 7 amended); 0.19.0 (2026-10-22): a restart of the stack carries its lanes over (ADR-0052), the agent skills where the stack file says, a node's policy says what part of what is free it offers (ADR-0031 amended); 0.20.0 (2026-10-23): macOS in preview with Homebrew (ADR-0054), UDP ports of a lane on a node relayed (ADR-0056), announced ports moved with their lane (ADR-0057); 0.21.0 (2026-10-23): a tray icon on every desktop, a client of the hub (ADR-0060), profiles that leave out containers (ADR-0058), a service's `stop` command (ADR-0059); 0.22.0 (2026-10-24): the dashboard opens in its installed app (ADR-0061); 0.23.0 (2026-10-26): what `prepare` makes kept in a store per machine (ADR-0062), Compose told the lane (ADR-0064) ([CHANGELOG](../CHANGELOG.md))
- **Date:** 2026-10-14, status updated 2026-10-26
- **Inputs:** [problem-statement.md](problem-statement.md), [ADR index](adr/README.md)

This document is the planning artefact. Decisions it relies on are recorded as
ADRs; where an ADR is still `Open`, the plan names both branches and does not
build on either.

## 1. Goals

1. Remove the three root causes in the problem statement: shared build/run
   output, restart-on-every-change, opaque orchestration.
2. The main slot never regresses to a state that does not build or start.
3. "Which code is running?" has an answer, everywhere, at a glance.
4. Several concurrent writers (IDE, terminals, AI agents, worktrees) are the
   design case.
5. Windows-native first, without closing the door on WSL.
6. No lock-in: every stack devstack runs also runs with Compose and the
   stack file's commands alone ([ADR-0015](adr/0015-devstack-is-optional.md)).

## 2. Architecture

```
                ┌──────────── devstackd (Rust, 127.0.0.1) ─────────────┐
 devstack CLI ─►│ HTTP API ── event stream (SSE) ── static dashboard   │◄─ browser
 MCP (stdio) ──►│                                                       │
                │  Infra     Builder              Supervisor            │
                │  ───────   ─────────────────    ───────────────────── │
                │  compose   watch → debounce     slots: main + cands   │
                │  up/wait   snapshot → build     health, logs, ports   │
                │  health    cancel superseded    job-object trees      │
                │            artifact store       (front: ADR-0010)     │
                └───────┬───────────────┬──────────────────┬───────────┘
                        │               │                  │
                 docker compose   .devstack/lanes/   .devstack/artifacts/
                 (infra plane)    <lane>/src (snap)  <svc>/<build-id>/
```

**Status 2026-10-18:** the diagram is the plan of phase 1. As built, the
daemon serves no dashboard: a **hub** (one per user, `127.0.0.1:4250`, part
of the same binary) serves it for every stack, forwards
`/hub/stacks/{id}/api/...` to each daemon with the token, and carries the
daemons' event streams to the browser over one WebSocket
([ADR-0021](adr/0021-a-hub-serves-the-dashboard.md)). The daemon's own API
still streams server-sent events, which the CLI reads directly. Lanes, their
infrastructure and the pool are in §3.5 and [pool.md](pool.md).

### 2.1 Planes

| Plane | Owner | Lifetime | Decision |
|---|---|---|---|
| Infrastructure (databases, brokers, schedulers, telemetry sink) | Docker Compose, driven by the daemon: the project's existing deployment Compose files plus a dev overlay | Long-lived; survives daemon restarts | [ADR-0004](adr/0004-infrastructure-is-delegated-to-compose.md) |
| Configuration of native services | The Compose service each one replaces, with explicit host rewriting | Read at every start | [ADR-0014](adr/0014-native-services-take-their-configuration-from-compose.md) |
| Build | Daemon | Per change | [ADR-0002](adr/0002-processes-run-from-immutable-artifacts.md), [ADR-0003](adr/0003-changes-flow-through-candidates.md) |
| Run (application processes) | Daemon | Per promotion | [ADR-0008](adr/0008-background-workers-run-only-in-the-active-slot.md), [ADR-0009](adr/0009-the-daemon-owns-every-process-tree.md) |

### 2.2 Components

- **devstackd:** single binary. Holds the stack model, the build queue, the
  supervisor and the HTTP API. Binds loopback only.
- **devstack (CLI):** thin client of the HTTP API. Can start the daemon
  (`devstack up`) and attaches to it otherwise.
- **MCP server:** `devstack mcp` (stdio), a client of the same API. Most
  tools are one API endpoint; some do what the CLI does around it (`up`
  also starts the daemon and Docker, `doctor` and `diagnostics` read the
  machine and the daemon's log) ([ADR-0007](adr/0007-one-http-api-is-the-control-surface.md)).
- **Dashboard:** Vue 3 SPA served by the daemon from the same origin
  ([ADR-0006](adr/0006-dashboard-vue-same-origin.md)); as built, served by
  the hub for every stack ([ADR-0021](adr/0021-a-hub-serves-the-dashboard.md)).

## 3. Core flows

### 3.1 `devstack up`

1. Load the stack file plus profile overlay plus untracked secrets file;
   validate (unknown keys are errors).
2. Port preflight: every fixed port is free, or owned by the daemon's own
   children; otherwise report the owning process by name and stop.
3. For every Compose group in declared order: create declared external
   networks, then `docker compose -p <name> -f <deployment files> -f <dev overlay>
   --env-file <dev env> up -d --wait --no-deps <selected services>`, with a
   per-service timeout. Failure names the group, the service and its last health
   output.
4. For every application service: snapshot → build → artifact (§3.2), unless an
   artifact for the current input hash already exists.
5. Run jobs (`kind = "job"`, e.g. schema migrations) to completion in
   dependency order; a non-zero exit stops the start with the job's log tail.
6. Start long-running services into the **main** slot with workers enabled;
   wait for health with timeout. A service that names a Compose service gets its
   environment from it ([ADR-0014](adr/0014-native-services-take-their-configuration-from-compose.md)).

### 3.1a What lives where (infra plane)

| Kind | Where | Example |
|---|---|---|
| Infrastructure that is also deployed | Existing deployment Compose file, unchanged | database, scheduler, renderer |
| Application containers that run natively in development | Same file, **parked** by the dev overlay (`profiles: [container-app]`) | API, migrator, SPA host |
| Development-only replacements | Dev overlay | telemetry dashboard instead of telemetry store, local database instead of an external one |
| Services of other deployment hosts | Dev overlay, `extends: { file, service }` | webhook ingress, portal database |
| Mock servers with code in the repository | Stack file, native service | third-party API mocks |
| Image-only mocks | Dev overlay | an off-the-shelf fake SMTP server |

The overlay is the complete, reviewable list of differences between deployment
and development. ADR-0004 lists the two Compose merge behaviours an overlay has
to handle (parked services under hard `depends_on`, `extends` carrying
references).

### 3.2 Change → candidate → main

1. **Watch** the paths a service declares. Events are coalesced over a quiet
   period (default 2 s, configurable).
2. **Snapshot** the lane's source into `.devstack/lanes/<lane>/src`: tracked
   files plus untracked non-ignored files. Only changed files are copied, and
   copied files get the current time as mtime, so incremental builds see them as
   newer than their outputs. The snapshot directory persists, so `obj/` and
   other incremental state stay warm.
3. **Input hash** over the service's declared inputs = build id.
4. **Build** in the snapshot directory with the normal output directories,
   then **publish** by copying the output into
   `.devstack/artifacts/<service>/<build-id>/` (ADR-0002). If a newer change for the same
   lane and service arrives, the running build's process tree is killed and the
   cycle restarts at step 2. A superseded build is recorded as `superseded`,
   not `failed`.
5. **Candidate start:** the artifact starts on a free port with workers
   disabled ([ADR-0008](adr/0008-background-workers-run-only-in-the-active-slot.md));
   health checked directly against that port.
6. **Promotion gate** ([ADR-0013](adr/0013-schema-changes-hold-promotion.md)):
   automatic unless the build changes declared schema inputs, in which case the
   candidate waits for an explicit `devstack promote`.
7. **Promotion** per [ADR-0010](adr/0010-promotion-mechanism.md): the
   reverse proxy on the fixed port switches to the candidate (option B,
   measured).
8. The previous main artifact is kept. `devstack rollback <service>` promotes it
   again without a build. Retention: last N artifacts per service (default 5).

**Status 0.2.0:** steps 1-8 are implemented (`crates/devstack-daemon`:
`watch.rs`, the candidate pipeline and `replace` in `supervisor/`, `proxy.rs`),
with option B of ADR-0010; `restart` takes the same path. Phase 2 additionally
brought the hub and the dashboard (ADR-0021).

**Until candidates exist (phase 2)**, `restart` of a built service follows the
same rule in a simpler form: it builds first and replaces the running instance
only if the build succeeded; a failed build leaves the running instance as it
was. Compiler messages are mapped from the snapshot back to repository paths,
so they stay clickable.

### 3.3 Build identity

Every process the daemon starts receives `DEVSTACK_BUILD_ID`,
`DEVSTACK_LANE` and `DEVSTACK_SLOT` in its environment. The daemon shows them in
status. Downstream applications are encouraged, not required, to expose them
(e.g. a `/version` endpoint, a response header, a `version.json` next to the SPA
bundle), so a browser or test can assert which build it is talking to.

### 3.4 Frontend dev server

A hot-reloading dev server is itself a builder and a server in one process, so
the candidate model does not fit it directly. Plan:

- **Main slot:** the dev server runs supervised (health, logs, restart on
  config-file changes such as path aliases, which it cannot pick up live).
- **Immutable variant:** `kind = "static"` builds the production bundle into an
  artifact and serves it on its own port. Intended for end-to-end runs that
  must pin a build id. **Built (0.6.0):** the artifact is served by
  `devstack serve-static` as the service's process, so candidates, the proxy
  and the health check work as for any service.

### 3.4a Hot reload of backend services

devstack does **not** use `dotnet watch` (or any in-place watcher) for its
services by default. It builds in the source tree's own `bin/`, from which the
running process is loaded, and restarts on every change whether the build
succeeds or not: root causes A and B of the problem statement. The default is
the candidate pipeline: build in the snapshot, start only a healthy build
(ADR-0002, ADR-0003); an incremental change costs about 2-3 s of build plus the
start. What it does not give is in-process code replacement (.NET Hot Reload,
edit and continue) that keeps the process and its state.

When a developer wants that for a debugging session, a service can be switched
to a **hot-reload mode**: it runs as a `devserver` with the toolchain's own
watcher.

```toml
[services.api]
kind = "devserver"
cwd  = "src/Api"
run  = "dotnet watch run --non-interactive"
port = 5000
```

What the mode gives up, deliberately, for that service: no snapshot, no
artifact and no build id (it runs from the source tree); no candidates and no
"a broken build never replaces a working one"; the file locks and duplicate
launches of the problem statement come back for it. It is a per-service choice
for a session, not a stack-wide default.

**Built (0.5.0):** `devstack hot-reload api on|off` switches a built service
into the mode at runtime and back, from `[services.api.hot_reload]` (`run`,
`cwd`) in the stack file; the dashboard and the MCP tool `hot_reload` do the
same. While it is on, the watcher and candidates leave the service alone,
`restart` starts the watcher afresh, `promote` and `rollback` refuse.

### 3.5 Lanes

A lane is a source root (the main working tree or another worktree) plus its
snapshot, artifacts, candidates and per-lane resources. Exactly one lane owns
the main slot at a time; `devstack promote --lane <name>` moves it.

Each lane runs its own Compose projects, so its data is its own; only what
the stack file marks `shared` (a telemetry dashboard, a fake SMTP server)
comes from the main tree ([ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md),
which settled [ADR-0012](adr/0012-shared-infrastructure-between-lanes.md)). A
non-main lane runs its candidates **without workers** and therefore never talks
to coordinators that dispatch work (schedulers, pull queues).

**Built (0.5.0, [ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md)):**
a lane is a supervisor in the main working tree's daemon; every linked
worktree is one (`git worktree list` decides; git creates and removes them,
devstack follows), `devstack up` in it starts it, and it runs from a copy of
the worktree, never in it; its ports move by `[lanes] port_step` times its
number, and it runs its own Compose projects with the same offset, sharing
only what the stack file marks `shared` ([ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md));
`promote --lane` forwards the fixed ports of the proxied services and moves
the workers.

**Built (0.9.0 to 0.9.8, confined in 0.10.0, [ADR-0025](adr/0025-a-lane-can-run-on-another-node.md) to
[ADR-0037](adr/0037-telemetry-goes-to-the-pools-collector-with-consent.md)):** a
lane runs on another machine of a pool (`devstack up --on <node>`), its ports
forwarded here; it moves, is shared with others in the pool, and on a
dedicated node takes the main ports. How to set up and use a pool:
[pool.md](pool.md).

**Built (unreleased, [ADR-0047](adr/0047-the-dashboard-makes-a-lane-from-a-branch.md)):**
the stack header lists the repository's branches; the dashboard makes a lane
from one (the hub runs `git worktree add`, the folder beside the
repository's other worktrees), or from a new branch, and removes a stopped
lane's worktree. The CLI and the MCP server still leave that to git; the
main working tree's checkout is never changed.

**Built (unreleased, [ADR-0055](adr/0055-a-lane-without-a-stack-file-says-so-and-can-be-given-one.md)):**
a lane whose branch has no stack file runs with the main tree's, on a node
too (it is synced there); the branch picker and the lane's page say so, and
the page copies the main tree's file into the worktree or starts a coding
agent that adapts it to the branch.

### 3.6 Daemon crash, restart and update

The daemon takes the whole application plane down with it and never re-attaches
([ADR-0016](adr/0016-a-daemon-restart-is-a-clean-restart.md)). A restart takes
the OS lock on the state directory, removes what a previous instance may have
left (Linux process groups / cgroups, incomplete builds), checks the Compose
groups without restarting them, and starts every service from the artifact that
was last promoted to main, without building. An update is the same path after a
graceful stop.

## 4. Stack file (schema version 1, draft)

> The reference is [stack-file.md](stack-file.md), kept in step with the
> code (and `devstack schema`). The tables below are the original draft and lack
> what came later (`[lanes]` with `keep_data`, `[profiles]`, `compose.shared`,
> `kind = "static"`, `prepare`, `hot_reload`, `rebuild`, `proxy`, `timeout`,
> `data`, lane placeholders).

Format: TOML, `devstack.toml` in the repository root
([ADR-0017](adr/0017-stack-file-is-toml.md), confirmed). No environment
profiles in version 1; `[profiles]` of
[ADR-0041](adr/0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) say how
the one stack runs for a purpose (ADR-0017, amended). A complete example is
[`examples/devstack.toml`](../examples/devstack.toml).

### 4.1 Top level

| Key | Type | Meaning |
|---|---|---|
| `version` | integer | Schema version, `1` |
| `stack.name` | string | Display name; prefix for daemon state |
| `stack.requires` | string | Oldest devstack that can run the file (`"0.1.0"`); checked before the strict parse, so an old devstack asks to be updated |
| `stack.env_files` | list | Env files `${VAR}` resolves against in every service and command; a service's Compose group files and the process environment win ([ADR-0014](adr/0014-native-services-take-their-configuration-from-compose.md), acceptance finding) |
| `compose.<group>` | table | A Compose project (ADR-0004) |
| `services.<name>` | table | A native process managed by devstack |
| `commands.<name>` | table | An operation users and agents can run |
| `groups.<name>` | table | A view over services, Compose services and commands (§4.4a) |

### 4.2 `compose.<group>`

| Key | Meaning |
|---|---|
| `project` | Compose project name (fixed, never derived from a path) |
| `files` | Deployment Compose files first, then the dev overlay |
| `env_files` | Later files win; typically tracked defaults, then an untracked `.env` |
| `networks` | External networks created before `up` |
| `services` | Services started (`--no-deps`); everything else stays down |
| `after` | Groups that must be healthy first |
| `restart_on.<service>` | Paths whose change restarts that container |

### 4.3 `services.<name>`

| Key | Meaning |
|---|---|
| `kind` | `service` (default), `job` (runs to completion), `devserver` (builds and serves in one process, plan §3.4), `tool` (long-running, interactive, no health) |
| `build` | Command run in the lane's snapshot, with the toolchain's normal output directories (ADR-0002); absent for `devserver`/`tool`. It runs again and again in the same snapshot and must be repeatable (incremental build tools are) |
| `output` | Directory (relative to the repository) copied into the artifact after a successful build |
| `run` | Command started from the artifact (or `cwd` for `devserver`/`tool`) |
| `cwd` | Working directory relative to the repository |
| `watch` | Paths whose change starts a candidate build (ADR-0003) |
| `restart_on` | Paths whose change restarts a `devserver` |
| `port` | Fixed port of the main slot; `{port}` resolves to it (or a candidate's port) |
| `health` | `{ http, timeout, insecure_tls }` |
| `after` | Services or `compose.<group>.<service>` that must be healthy (jobs: completed) first |
| `autostart` | `false` = started on demand only |
| `platforms` | Restrict to `windows`, `linux`, `macos` |
| `compose` | `{ group, service }` whose environment is the base (ADR-0014); `inherit = false` makes it a reference only, `ignore` lists variables left out on purpose, `check --compose` reports the rest |
| `rewrite_hosts` | Compose services rewritten to published host addresses (ADR-0014): `<service>` of the own group, `<group>.<service>` of another |
| `env` | Wins over the Compose environment |
| `workers.env` | Worker switches, `{is_main}` = `true`/`false` (ADR-0008) |
| `ensure_dirs` | Directories created before start |
| `promote` | `{ auto, hold_if_changed }` (ADR-0003, ADR-0013) |

### 4.4 `commands.<name>`

| Key | Meaning |
|---|---|
| `description` | Shown in CLI, MCP and dashboard |
| `confirm` | Confirmation text; the command does not run without it being acknowledged |
| `requires` | Services that must be healthy for the command to be enabled |
| `steps` | Ordered list: `{ exec, cwd }`, `{ http = "METHOD url", headers, timeout }`, `{ start }`, `{ stop }`, `{ restart }`, `{ run }` (a job) |

Anything beyond these steps (parsing a response, opening a browser) belongs in a
script the downstream project owns and calls with `exec`.

### 4.4a `groups.<name>` (views, not hierarchy)

Groups organise services, Compose services and commands for display and for
acting on several at once. They are **labels, not parents**: no member needs
another member to exist, a member may be in several groups, and nothing starts
or stops because of the group it is in (dependencies are `after`, nothing
else). Members that are in no group appear under "Other".

| Key | Meaning |
|---|---|
| `label` | Display name (default: the group's key) |
| `members` | `<service>`, `compose.<group>.<service>`, `command.<name>` |
| `order` | Sort position in the dashboard (default: alphabetical) |

```toml
[groups.portal]
label   = "Customer portal"
members = ["portal-api", "portal-spa", "compose.app.portal-db", "command.portal-onboard"]

[groups.telephony]
members = ["telephony-mock", "compose.app.webhooks", "api"]   # api is in two groups
```

CLI and API accept a group wherever they accept a service
(`devstack restart portal`, `devstack start portal`).

### 4.5 Interpolation

Closed set; an unknown placeholder is an error.

| Placeholder | Value |
|---|---|
| `{repo}` | Absolute path of the lane's source root |
| `{artifact}` | Artifact directory of the build being started (used in `run`) |
| `{port}` | Port of the instance being started |
| `{is_main}` | `true` in the main slot, `false` otherwise |
| `{service.<name>.url}` | `http(s)://127.0.0.1:<port>` of that service's main slot |
| `{compose.<group>.<service>.<container port>}` | `http://127.0.0.1:<published host port>` |
| `{service.<name>.port}`, `{compose.<group>.<service>.<port>.host}` / `.port` / `.address` | One part of an address |
| `{lane}`, `{lane_suffix}` | The lane's name; `_<lane>` in a lane, empty in the main tree |
| `{compose.<group>.project}` | The group's Compose project as the tree runs it: `<project>`, `<project>-<lane>` in a lane |
| `${VAR}`, `${VAR:-default}` | From the process environment and the group's env files |

Resolution order: `{…}` first, then `${…}`, so a default may contain a placeholder.

### 4.6 Deliberately not in version 1

Profiles for other environments and layering of stack files (a purpose
profile, ADR-0041, is something else), conditionals and expressions, per-lane
overrides, secrets management of its own (secrets come from env files, which the
downstream secret tooling fills).

### 4.7 Findings from writing the first real stack file

The reference stack (12 native services, 5 commands, 2 Compose groups) fits the
schema. Four points it surfaced, now part of the draft:

1. **Placeholders inside env defaults** are needed
   (`"${AI_URL:-{service.ai-mock.url}/v1}"`): `{…}` placeholders are resolved
   first, then `${…}`.
2. **Two services may share a port** when one of them is on demand (a dev server
   and a container-like host for the same SPA). Allowed; starting one while the
   other holds the port is refused with the owner named.
3. **Native services that are not parked Compose services** (a portal API that
   lives in another host's Compose file) carried many `env` entries. Pulling
   that service into the dev overlay with `extends` and parking it makes it a
   `compose = { … }` source as well (ADR-0014) and removes those entries.
4. **Commands that parse responses or open a browser** become small scripts in
   the downstream repository (`exec`), as §4.4 intends; no new step types.

## 5. Control API (sketch)

> The API as built, with the hub's and the pool's routes:
> [dashboard-api.md](dashboard-api.md). The sketch below is the original plan.

| Endpoint | Purpose |
|---|---|
| `GET /v1/stack` | Services, slots, lanes, build ids, health, ports |
| `GET /v1/events` | SSE stream: state changes, build progress, log lines |
| `GET /v1/builds?service=` | Build history (queued / running / superseded / failed / succeeded) |
| `GET /v1/services/{name}/logs?slot=&tail=` | Log tail |
| `POST /v1/services/{name}/promote` / `rollback` / `restart` / `stop` / `start` | Lifecycle |
| `POST /v1/commands/{name}` | Run a command from the stack file; returns a run id |
| `GET /v1/commands/{name}/runs` / `…/runs/{id}` | Command run history and output |
| `GET /v1/usage?by=service\|group\|lane` | Resource usage, current and recent (§12) |
| `POST /v1/infra/{service}/restart` | Container restart (e.g. after a config change the container cannot see) |

**Security:** loopback only, a token file for CLI and MCP, a one-time code
exchanged for an `HttpOnly` cookie for the dashboard, `Host` and `Origin`
checks ([ADR-0019](adr/0019-control-api-security.md)).

## 6. Dashboard scope (phase 2 and later)

Delivery: a browser page served by the daemon, opened with `devstack ui`; status
icon and notifications through a tray in the daemon binary ([ADR-0018](adr/0018-dashboard-is-a-browser-page.md)).
The notifications came without the tray, from the hub
([ADR-0045](adr/0045-the-hub-notifies-the-desktop.md)); the tray came as a
client process the hub starts, on every platform
([ADR-0060](adr/0060-the-tray-is-a-client-of-the-hub.md)).

- **Overview by group** (§4.4a): one panel per group, ungrouped members under
  "Other", a member shown in every group it belongs to. Per member: state,
  health, slot, build id, ports, lane, resource usage (§12). Collapsible, the
  layout remembered per browser.
- **Console per service:** live output of every process instance (main slot and
  candidates, selectable), ANSI colours rendered, search, filter by build id and
  time, jump to the line where the last failure started. The same view for
  Compose services (their `docker compose logs`).
- **Actions:** start, stop, restart, promote, rollback per service and per group;
  the stack file's `commands` (e.g. reset the database) as buttons on the group
  they belong to and in a command palette. A command with `confirm` asks first;
  while it runs its steps and their output stream into a panel; every run is kept
  with start, end, exit code and output (`.devstack/logs/commands/<name>/`), so
  "what did the last reset do" has an answer.
- **Build timeline** per service, including superseded and held builds.
- **Resource view** (§12): per service, group and lane, now and over the last
  minutes.
- **Lane switcher.**
- Explicitly not: a telemetry viewer. Traces and metrics go to an OTLP sink
  defined in the downstream Compose file.

**Status 2026-10-15:** served by the hub, not the daemon ([ADR-0021](adr/0021-a-hub-serves-the-dashboard.md)),
as an installable page that keeps the last known state offline. Built: overview
by group, a stack switcher, the "not yet active" panel per service, the console
of the current instance (live, filter, pause), service and stack actions,
commands with confirmation and their run history, resources, the events
timeline, Docker and stack-file banners, container logs, earlier logs of a
service, sortable tables, actions per group, the jump to the last failure, a
candidate's console, a build timeline per service, a command palette, the lane switcher, the tray (ADR-0060).

## 7. Phases

Each phase ends with a usable state. Exit criteria are measured on the first
downstream stack.

| Phase | Content | Exit criterion | Status |
|---|---|---|---|
| **0: Spikes** | (a) process ownership, see §7.1; (b) build cancellation, see §7.2; (c) snapshot sync cost for the reference tree; (d) platform benchmark for [ADR-0011](adr/0011-platform-windows-native-first.md) | Each spike answers its question with numbers, recorded in the relevant ADR | Done (2026-10-14) |
| **1: Run plane + CLI** | Stack file, compose driver, `devstack check`, `devstack env --export`, port preflight, artifact builds on demand (`devstack build api`), main slot, jobs, health, logs, job-object ownership, build-id env, CLI, MCP | Reference stack runs end to end without the previous orchestrator; no file-lock failure possible by construction | Done, released as 0.1.0 (§7.3) |
| **2: Candidate pipeline** | Watch, debounce, supersede/cancel, candidates without workers, promotion gate, rollback, SSE events, first dashboard | A broken edit never replaces main; three concurrent writers produce at most one running build per service | Done, released as 0.2.0 (§7.4) |
| **3: Lanes** | Worktree lanes, per-lane resources, `promote --lane`, static frontend artifacts for e2e | Two worktrees run side by side without touching each other's database or the main slot | Done: lanes per worktree (0.5.0 to 0.7.0, ADR-0024), each with its own infrastructure (0.8.0, ADR-0030), `kind = "static"` (0.6.0); lanes on other machines of a pool (0.9.x, ADR-0025 to ADR-0037, [pool.md](pool.md)) |
| **4: Adoption** | Migrate the downstream stack, delete its old orchestrator, publish | Downstream runbook shrinks to the stack file and a page of notes | Under way: the reference stack runs on devstack and its runbook is a generated skill plus one page; its old orchestrator stays as a fallback for now; public since 2026-10-15 with a landing page on GitHub Pages |

### 7.1 Spike (a): process ownership

A small Rust program (not yet the daemon) that starts process trees the way the
daemon will and answers, on Windows and Linux/WSL:

| # | Test | Pass when |
|---|---|---|
| a1 | Start `dotnet run` (app with a background worker) and `npm run …` (node under npm under a shell) in a Job Object / process group | Both trees are listed as one unit per service |
| a2 | Graceful stop: deliver Ctrl+C to one service's console group only | That service runs its shutdown handlers (marker written on stop); the other service keeps running |
| a3 | A process that ignores Ctrl+C | Terminated after the timeout, whole tree gone |
| a4 | Kill the spike program hard (`taskkill /F`, `kill -9`) | **No** process of either tree survives (Windows by kernel; Linux after the restart cleanup) |
| a5 | A child inherits handles (e.g. a shell that spawns further shells) | The job still closes on a4: the job handle is not inheritable |
| a6 | A child requests breakaway (`CREATE_BREAKAWAY_FROM_JOB`) | Refused; the child stays in the job |
| a7 | Tools that create their own job objects inside ours (test runner, `dotnet watch`) | Nested jobs work; a4 still holds |
| a8 | Restart after a4: lock file reacquired, Linux cleanup by cgroup and by recorded process group + start time | No stale lock; only recorded processes are killed (a decoy process with a reused PID survives) |

Results go into ADR-0009 and ADR-0016.

**Status 2026-10-14:** Windows done, a1-a8 pass, with `CTRL_BREAK` plus a hidden
daemon console as the stop method; ConPTY fails to stop node (open). Linux done
in a container: a shim per service plus a cgroup gives the same guarantee. See
`spikes/process-ownership/README.md`.

### 7.2 Spike (b): build cancellation

ADR-0003 cancels a superseded build by killing its process tree. That only works
if a killed build leaves nothing behind that breaks or slows the next one. The
spike drives the reference stack's API build and its SPA production build from a
small harness, kills them at defined points and builds again.

| # | Test | Pass when |
|---|---|---|
| b1 | Kill the .NET build during restore, during compilation and during the output copy (each 10×, random offset within the phase) | The following incremental build succeeds every time, and its output equals a clean build's output (file list and assembly hashes, excluding timestamps) |
| b2 | Time from cancel to "no process of the build alive, no file of the lane locked" | Under 2 s; the next build can start immediately |
| b3 | Duration of the build after a cancel, compared with a normal incremental build of the same change | Within 20 % (a cancel must not regularly turn the next build into a cold one) |
| b4 | Graceful cancel (Ctrl+C to the build's console group) versus hard kill | Recorded; if graceful is reliably faster to resume, the builder tries it first with a short timeout |
| b5 | Long-lived build servers (MSBuild node reuse, the shared C# compiler server): killed with every cancelled build when they live in the build's job, versus living in a separate daemon-wide job | Recorded with build times for both; decides whether build servers get their own job |
| b6 | SPA production build: kill during bundling, rebuild | Succeeds; the build cache is either reused or detected as corrupt and rebuilt, never silently wrong |

Results go into ADR-0003 (cancellation) and ADR-0009 (job layout for build
servers).

**Status 2026-10-14:** done on Windows. All cancelled builds resumed correctly and
byte-identically; hard kill recommended; build servers must outlive a build
(open design question, ADR-0009). See `spikes/build-cancel/README.md`.

### 7.3 Phase 1 work packages

Starts once the ADRs phase 1 builds on are accepted (0002, 0003, 0004, 0007,
0008, 0009, 0011, 0013, 0014, 0015, 0016, 0019). One Cargo workspace, one binary
(ADR-0005); crates in dependency order:

| # | Crate | Content | Taken from |
|---|---|---|---|
| 1 | `devstack-config` | Stack file model, strict loading, validation, interpolation (§4.5), groups (§4.4a), JSON Schema | ADR-0017, plan §4 |
| 2 | `devstack-proc` | Process ownership per platform: Windows job + process group + hidden console; Linux shim + cgroup + process group; restart cleanup; OS lock; resource usage per tree (§12) | Spike (a) code |
| 3 | `devstack-compose` | Compose groups: `config --format json`, `up --wait --no-deps`, `ps`, restart, external networks, published ports | ADR-0004 |
| 4 | `devstack-env` | Environment composition from Compose service + stack file + devstack variables, host rewriting, `--export dotenv` | ADR-0014, ADR-0015 |
| 5 | `devstack-build` | Snapshot sync, build in lane, publish by copy, build ids, shared build job and cancellation (ADR-0020), retention | ADR-0002, spike (b), (c) |
| 6 | `devstack-daemon` | Supervisor (main slot, jobs, `after`, health, ports), state persistence, HTTP API with auth, SSE, logs, command runs, usage sampling (§12) | ADR-0007, 0016, 0019, plan §5, §11 |
| 7 | `devstack` (bin) | CLI subcommands, `devstack mcp`, `devstack check`, `devstack env` | ADR-0007, 0015 |

**Status 2026-10-14:** WP1 to WP6 done; WP7 done with `devstack mcp`. The MCP
server speaks JSON-RPC over stdio itself (protocol versions 2024-11-05 to
2025-06-18, tools only) rather than through an SDK crate: three methods do not
justify a dependency that moves faster than the protocol. Each tool runs on its
own thread, so `ping` is answered during a restart that builds for minutes.
Agent-supplied names are checked to be plain names before they become path
segments. A command's confirmation is never given by the server: the tool
returns the text, the agent has to ask.

**Acceptance 2026-10-14:** the reference stack (14 containers in two Compose
groups, 9 native services, 7 commands) runs end to end under devstack on
Windows: `devstack up` from a warm artifact store in under a minute, every
autostart service ready, its jobs, webhooks, seeds and telemetry working. The
acceptance found five defects, all fixed with tests:

- `up` stopped at the first failed service; now only what comes `after` it is
  held back.
- A finished job was reported as `exited` (read as a crash) instead of
  `completed`.
- Health checks and the port check knew only `127.0.0.1`; a dev server on
  `localhost` may listen on `::1` only (Node on Windows).
- `${VAR}` resolved only for services linked to a Compose service; hence
  `[stack].env_files` and the ADR-0014 finding.
- Linux: a tree started from a short-lived thread died with it
  (`PR_SET_PDEATHSIG`, ADR-0009), found earlier by the daemon integration test.

Hardening right after, against the same stack (code changes, an agent over MCP,
a hard daemon kill, stopped containers):

- `start` and `restart` ignored `after`: a restart after a new migration skipped
  the migrator and the new instance failed its pending-migration guard. Now
  `start` starts what is not ready, and `restart` reruns the jobs the service
  comes after, before the old instance is replaced; a failing job leaves it.
- A failed .NET build showed its last 15 lines, which were NuGet warnings; the
  message now shows the error lines.
- Container states were read only at `up`/`down`; a stopped container showed as
  running. They are re-read every 5 s (`compose ps`), each change is an event.
- `http` steps could not send a body.
- The environment link gained a reference mode with a drift report
  (ADR-0014, `inherit = false`).

Not tested: a machine reboot and a Docker Desktop restart.

Phase 1 delivers builds on demand and the main slot; candidates, auto-promotion
and the dashboard are phase 2. Exit criterion unchanged: the reference stack
runs end to end without its previous orchestrator.

### 7.4 Phase 2 status

**Done (0.2.0, 2026-10-15):** watcher with debounce and supersede, candidates on
internal ports behind a daemon-owned proxy ([ADR-0010](adr/0010-promotion-mechanism.md),
option B, measured: no failed request across a switch, against 3.9 s of errors
for a restart in place), workers only in the main slot, the promotion gate,
`promote`, `rollback`, SSE events with a snapshot first, and the dashboard served
by a hub ([ADR-0021](adr/0021-a-hub-serves-the-dashboard.md)). Exit criterion: a
broken edit leaves main running (integration test `edits_become_candidates`);
a newer change cancels the running build, so one service has at most one build.

**Released as 0.3.0 (2026-10-15), from running the reference stack:**

- Docker engine check: `up` starts Docker Desktop, the hub reports the engine
  and starts it from the dashboard.
- Daemon and hub detach from the terminal (job objects of IDE terminals,
  inherited pipes); both stop within 2 s despite open event streams.
- The hub starts at logon and from a `devstack://hub/start` link; pairings
  survive a hub restart.
- Services carry their URL with the right scheme.
- `config_changed`: the status says when the stack file changed since the start.
- Agent skills generated from the stack file (`devstack skills install`) and a
  reference for the stack file (`docs/stack-file.md`).
- The diagnostic log of §11.
- The dashboard loads the current version on reload and follows a hub update.
- `devstack doctor`, `devstack self uninstall`, `GET /hub/ping` and the landing
  page (`site/`, GitHub Pages); licence MIT OR Apache-2.0, `cargo deny` in CI.

**Released as 0.4.0 (2026-10-15):** "Restart stack" in the dashboard when a
stack runs an older daemon than the installed devstack; dashboard links for
every published container port (`urls` in a Compose group overrides them); log
search across services (`/v1/logs?grep=`, `devstack logs --grep`, MCP `logs`);
`devstack update` and a release check in hub, dashboard and `doctor`;
`devstack init` and "Add a stack" in the dashboard (register a folder, write a
draft, or copy a prompt for a coding agent); stack file compatibility rules
([ADR-0023](adr/0023-stack-file-compatibility.md)).

**Released as 0.5.0 (2026-10-15):** "Choose folder…" in "Add a stack" shows the
system's folder dialog through the hub (`POST /hub/pick-folder`; Windows
Explorer picker, XDG portal on Linux); the CLI says when the daemon is older
than itself instead of an empty error; earlier logs of a service by build
and by time (`devstack logs api --build/--since/--list`, MCP `logs`,
"Earlier logs" in the dashboard), every instance's log names its build; container logs (`devstack logs <container>`,
a log button per container in the dashboard); every table in the dashboard
sorts by its column headers; start, stop and restart per group (dashboard,
`devstack restart <group>`, MCP); a jump to where the last failure started in
every console ("Last error", again for the ones before); a candidate's own
console while it starts (`logs/stream?build=`); a build timeline per service
(`devstack builds`, MCP `builds`, "Builds" in the service view), read back
from the diagnostic log; a command palette (Ctrl+K / Cmd+K) for views, stacks,
services, groups, containers and commands; the hot-reload switch (§3.4a);
`devstack init` for Go, Python (Django, FastAPI, Flask) and Spring Boot, with
unique ports and a hot-reload section for .NET; the diagnostic log also as OTLP
log records (`DEVSTACK_OTLP_ENDPOINT`); **lanes** (§3.5, ADR-0024): a `git
worktree` runs as a lane of the main tree's daemon with `devstack up`, ports
moved per lane, `promote --lane`, a lane switcher in the dashboard; "Add a
stack" scans a folder of repositories, and a fresh dashboard opens with that
scan.

**Released as 0.5.1:** terminal colour codes in service output are rendered
in the dashboard's console and stripped where they would show as noise (other
text views, `grep` matching, the MCP tools, `devstack logs` into a file or a
pipe); the hub's first start after `devstack update` gets 30 s instead of 10.

**Released as 0.6.0:** a hub started while the previous one still lets go
of its state lock (right after `devstack update` stopped it) waits for the
lock instead of exiting silently; what a hub prints before its log exists goes
to `hub.out`, and a start that fails says so. A container healthy inside Docker
whose published port Docker Desktop no longer forwards (seen after Docker
Desktop restarted: connections are accepted and closed at once) is found:
before a service that comes `after` it starts (the container is restarted
once), after `up`, in `devstack status`, `devstack doctor` and the dashboard.
`promote --lane` also moves dev servers: a TCP forward on their fixed port to
the lane's (TLS and WebSockets pass). Lanes share the main tree's artifact
store (a worktree on the same sources starts without building), and artifacts
are pruned for the first time: the newest five per service plus everything
some tree runs or may roll back to (ADR-0002 retention). `kind = "static"`
serves a frontend's production bundle from an artifact (§3.4).

**0.7.0:** the lanes are the repository's worktrees
(`git worktree list`): a new worktree is a lane at once, not started, a removed
one is gone with its state (ADR-0024, amended). Worktrees are git's: no
`lane add` / `lane remove`; the daemon follows `<git common dir>/worktrees` and
sends a `lanes` event, and the dashboard shows each lane under its stack,
named by its branch, the ones not started folded away. A lane's dev servers,
tools and hot-reload mode run from its snapshot, which follows the worktree
within 150 ms; `prepare` installs what the snapshot lacks (`npm ci`), so
`git worktree remove` works while the lane runs. `devstack lane stop` stops a
lane. `promote --lane` moves the ports
first and the main slot only when all of them moved; `--lane main` restarts
only what the main tree ran before; a dev server listening on `::1` only is
reached through the forward. Pruning keeps every build a started lane runs or
may go back to, and an artifact is renamed away before it is deleted. A lane
never restarts the main tree's containers, and a removed lane releases its
ports and threads. `[lanes] port_step = 0` is an error.

**0.8.0:** each lane has its own infrastructure: a Compose project per lane
with ports moved by its offset, fixed-name networks and volumes renamed, only
`shared` services from the main tree (ADR-0030). The dashboard lists stacks and
their worktrees in a sidebar, gives containers pages, and installs updates
with a click; `devstack ui --browser` pairs the browser you develop in.

**0.9.0 to 0.9.8: the pool** ([pool.md](pool.md)). A coordinator admits members
with mutual TLS and places lanes; node agents run them in Linux containers
with their own infrastructure (ADR-0025 to ADR-0031). `devstack up --on` syncs a
worktree to a node by content and forwards its ports here, its containers'
too; lanes move between machines with make-before-break, show their start
step by step, and start where you choose. The dashboard administers the pool
(ADR-0032), a node decides who runs what there with limits and a network
mode (ADR-0033), a machine belongs to several pools (ADR-0034), lanes are
shared with others in the pool (ADR-0035), the pool shows its capacity, GPUs
included, over time (ADR-0036), and sends telemetry to its collector only
with the member's consent (ADR-0037). A lane on a dedicated node takes the
main ports (0.9.7, ADR-0025 point 4). Tested end to end in CI: a coordinator,
a node agent and a lane in Docker (`tests/remote_lane.rs`). 0.9.8 made the
coordinator say when it cannot pass telemetry on.

**0.10.0 (2026-10-18): the review of 0.9.8.** A review of the whole
repository (2026-10-17) found the pool's boundaries weaker than its ADRs
said and two core promises not kept; the fixes are this release, detailed
in the [changelog](../CHANGELOG.md). A node confines what a lane asks of it
(the Compose model checked, the mirror never followed out of, limits on
every container, offline builds on `internet`/`none` nodes) and only nodes
an admin granted run the pool's lanes (ADR-0027, ADR-0032, ADR-0033
amended). A held candidate keeps later ones held (ADR-0013), a daemon
restart starts the last promoted builds without building (ADR-0016), one
build job serves a daemon and its lanes (ADR-0020), and commands of a lane
run in its copy (ADR-0024). The hub requires its own page for every request
with the cookie, forbids framing and sends the token only to a hub that
proves it knows it (ADR-0019, ADR-0021 amended). The stack file rejects
more (paths outside the repository, placeholders without a value), jobs
time out, `env --export dotenv` reads back, and releases carry a build
provenance attestation. The UI got unit tests (`npm test`).

**Not covered by an automated test:** a machine reboot, a Docker Desktop
restart, the `devstack://` link opened from a browser, an IDE terminal closing.

**0.11.0 (2026-10-19): lanes as a place, not a different stack.** Stopping
the main tree no longer stops its lanes (ADR-0024). A lane on a node keeps
its mirror, builds and volumes there for its next start, until the node's
retention ends, and `lane reset`/`lane prune` throw them away (ADR-0038). A
remote lane's containers show their output and load, through its node. A
shared lane can be shared to view and is then shown as one's own, read only
(ADR-0040). An admin has a server update its devstack from the dashboard
(ADR-0039). Profiles say how a tree runs for a purpose, an acceptance run
above all (ADR-0041). Lanes start from the main tree (CLI and MCP), a
service that exits by itself comes back, and a Compose port is judged only
once its container is settled.

**0.12.0 to 0.14.1 (2026-10-20/21): dev servers and placement.** A dev
server's rebuilds show like builds, read from its output (ADR-0042), and its
fixed port is devstack's, forwarded to its instance (ADR-0043). Sharing a
lane includes viewing it (ADR-0040 amended). A node's own lines reach the
pool's telemetry, and a server that cannot update itself says so
beforehand (ADR-0039). The coordinator places a lane where it starts warm
(ADR-0026 point 3), where a lane starts without `--on` is the machine's
choice, and a lane's start on a node shows its phases. The details are in
the [changelog](../CHANGELOG.md).

**0.15.0 (2026-10-21): the review of the whole code base.** A node runs
the Compose model it checked and nothing the lane changes afterwards,
refuses Compose keys it does not know, and takes a lane's projects down
with it (ADR-0033 amended); several node agents share one Docker engine
(ADR-0034), admission holds under load and disk is counted (ADR-0026,
ADR-0038). The supervisor keeps edits made while a service is exited and
stops made while a migrator runs; stack file paths stay in the
repository; the docs say what the code does. The details are in the
[changelog](../CHANGELOG.md).

**0.16.0 (2026-10-21): the desktop and the machine.** The hub notifies the
desktop of what needs attention, a toast on Windows, the session bus on
Linux, without a tray (ADR-0045, ADR-0018 amended), and shows whose lanes
run on this machine. Windows installs and updates through winget. The
dashboard opens a stack's folder in the user's own programs (ADR-0046),
gives agents links to a stack or lane, and stops a stack with its
containers. The share switch is removed: it reported capacity but never ran
a node agent (ADR-0031 amended; a switch that starts one is #171, Windows as
a node #168). The details are in the [changelog](../CHANGELOG.md).

**0.17.0 (2026-10-21): branches and main ports.** The stack header lists the
repository's branches; the dashboard makes a lane from one, or from a new
branch, and removes a stopped lane's worktree, while the main working tree's
checkout stays the user's (ADR-0047, ADR-0024 point 1 replaced in part for
the hub). Containers named in `main_ports` serve the main ports through a
forward devstack holds, so `promote --lane` works for a stack of containers
too (ADR-0048; UDP stays Docker's, #192). The pool places a lane by what it
took where it ran, disk included, with a floor from the stack file
(ADR-0049). GitHub Desktop opens a tree, and opened programs come to the
front on Windows (ADR-0046 amended). The details are in the
[changelog](../CHANGELOG.md).

**0.18.0 (2026-10-21): notices.** What the user should know of but no
request reports (a lane's worktree missing, a copy not brought up to date)
reaches the dashboard as a notice or an event instead of the diagnostic log
only (ADR-0050); a file watcher that misses edits is shown. A lane whose worktree
is removed elsewhere shows "removing", then "removed", and stays shown until
left (ADR-0024 point 7 amended). The stack header's path is a menu whose
programs can be arranged; `down --all --infra` removes every container. The
details are in the [changelog](../CHANGELOG.md).

**0.19.0 (2026-10-22): lanes carried over a restart.** "Restart stack"
switches the daemon to the installed devstack while lanes run: a lane on a
node keeps running, one here starts again from its builds, the main ports
go back to the lane that had them (ADR-0052). The agent skills go where the
stack file's `[agents] skills` says; the dashboard says when a repository
was set up with an older devstack and starts a coding agent with its
prompt (ADR-0046 amended). A node's policy says what part of its free CPU
and memory it offers (ADR-0031 point 3 amended). The details are in the
[changelog](../CHANGELOG.md).

**0.20.0 (2026-10-23): macOS in preview, UDP through the pool.** devstack
runs on macOS as a developer platform in preview, checked by CI on macOS
and by testers (ADR-0054); Homebrew installs it on macOS and Linux, and
arm64 Linux has builds of its own. A lane on a node relays its UDP ports,
a tunnel per client (ADR-0056); a port a container announces moves inside
its lane's container too (ADR-0057); `{compose.<group>.project}` lets a
command reach the tree's own containers. A lane whose branch has no stack
file says so (ADR-0055); whoever a lane is shared with connects to it
(ADR-0053). The details are in the [changelog](../CHANGELOG.md).

**0.21.0 (2026-10-23): a tray icon, lighter profiles.** The hub starts a
tray icon on Windows, Linux and macOS, a client of the hub that shows what
runs and what needs attention and starts or stops a stack (ADR-0060). A
profile can leave out containers, whole Compose groups and services
(ADR-0058); a service's `stop` command ends what its `run` started outside
its process tree (ADR-0059). The details are in the
[changelog](../CHANGELOG.md).

**0.22.0 (2026-10-24): the dashboard in its app.** With the dashboard
installed as an app in a Chromium browser, the tray, `devstack ui` and a
notification open it there instead of in a browser tab, on Linux and
Windows (ADR-0061). The tray's menu on Windows follows dark mode. The
details are in the [changelog](../CHANGELOG.md).

**0.23.0 (2026-10-26): one `npm ci` per lockfile, Compose told the lane.**
What `prepare` makes is kept in a store per machine and repository, keyed
by its inputs, the platform and the toolchain's version; the next lane or
build with the same key copies it instead of running `prepare` (ADR-0062,
measured: 5 s against 23 s for `npm ci` on NTFS). Every Compose call and
`exec` step is told `DEVSTACK_LANE` and `DEVSTACK_LANE_SUFFIX`, and a lane
on a node gives its native services its name too (ADR-0064). A lane on a
node keeps its lease through a long first sync and is said when a reboot
lost it. The store on a node and the worktree link of a lane on a node
(ADR-0063) follow. The details are in the [changelog](../CHANGELOG.md).

## 8. Open questions

| # | Question | Where it is tracked |
|---|---|---|
| O-1 | Stack file format: TOML, provisional | Resolved: TOML, confirmed 2026-10-15 after the reference stack ran end to end ([ADR-0017](adr/0017-stack-file-is-toml.md)); compatibility by [ADR-0023](adr/0023-stack-file-compatibility.md) |
| O-2 | Promotion mechanism: restart on fixed port vs. reverse proxy | [ADR-0010](adr/0010-promotion-mechanism.md): resolved, proxy (B), measured |
| O-3 | Windows-native vs. WSL | Resolved: Windows-native first, WSL only through Docker Desktop for containers, measured ([ADR-0011](adr/0011-platform-windows-native-first.md), accepted) |
| O-4 | Which infrastructure may be shared between lanes | Resolved: each lane its own, `shared` marks the exceptions ([ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md)) |
| O-5 | Licence | Resolved: MIT OR Apache-2.0; the repository is public since 2026-10-15 |
| O-6 | Default daemon port | Resolved: the daemon takes a free port and writes it to `.devstack/daemon.json`; the hub has a fixed one (4250, `DEVSTACK_HUB_PORT`) |
| O-7 | Lanes per worktree only, or also per agent session in one tree | Resolved: per worktree; agents work in worktrees ([ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md); owner, 2026-10-17: devstack never creates worktrees) |
| O-8 | Development values that are not secret (local passwords, dev API keys): a tracked `dev/.env.defaults` next to the untracked `dev/.env`, or everything through the downstream secret chain | Downstream decision; devstack supports several env files per group either way |
| O-9 | Minimum Docker Compose version (overlays rely on `!reset`, `!override`, `required: false`) | Resolved: 2.24.4, checked by `devstack doctor` |
| O-10 | How build servers outlive a single build but die with the daemon (spike (b)) | [ADR-0020](adr/0020-builds-share-one-long-lived-build-job.md), accepted |

## 9. Risks

- **Scope creep into a general orchestrator.** Guard: compose owns containers;
  devstack never schedules, places or networks containers itself.
- **Windows process control is harder than it looks** (console control events,
  job objects nested by other tools, processes that ignore Ctrl+C). Phase 0
  spike (a) exists for this.
- **Snapshot cost on large trees.** Phase 0 spike (c); fallback is building in
  place for services whose output directory is not the run directory.
- **A reverse proxy reintroduces an opaque layer** if chosen. ADR-0010 lists
  the conditions it must meet.

## 10. Testing devstack itself

devstack manages processes, files and ports on three operating systems; most of
its failures are platform behaviour, not logic. The test layout follows that.

| Layer | What | Where it runs |
|---|---|---|
| Unit | Stack-file parsing and validation, interpolation (§4.5, including the resolution order), env composition (ADR-0014), promotion gate, snapshot diff | `cargo test`, every OS |
| Platform | Process ownership and stop (the spike (a) scenarios, kept as tests), file lock, snapshot sync with real files | `windows-latest`, `ubuntu-latest` (cgroup scenarios in a privileged container job) |
| Integration | A small example stack in the repository (`examples/fixture-stack`: a .NET API, a node dev server, a job, one Compose service) started end to end: candidate, promotion, rollback, daemon crash and restart | `windows-latest` and `ubuntu-latest`, nightly and on release branches |
| Fault injection | A service that hangs on stop, a build that is cancelled mid-way (spike (b) cases), a port held by a foreign process, a Compose service that never becomes healthy | Same as integration |
| Downstream | The first downstream stack, run by its developers | Their machines; feedback as issues |

Rules: no test talks to the network beyond the container registry; platform
tests assert on the outside (process liveness, files, exit codes), like the
spikes, not on internal state.

**As built:** `cargo test --workspace` runs everything that needs neither
the .NET SDK nor Docker nor a node. The rest is `#[ignore]`d with the
reason, so `cargo test` lists it as ignored; it runs with `-- --include-ignored`
and its variable set (an included test whose variable is not set returns at
once). CI sets them where the job has what they need.

| Variable | Tests | Needs |
|---|---|---|
| `DEVSTACK_INTEGRATION=1` | `crates/devstack/tests/daemon_lifecycle.rs`: the fixture stack end to end (candidates, promotion, restore after a hard kill, lanes, a hanging job) | the .NET 10 SDK |
| `DEVSTACK_INTEGRATION_COMPOSE=1` | the same file's Compose variants (`lifecycle_with_compose`, `a_lane_has_its_own_containers`) | the .NET 10 SDK and Docker with Linux containers |
| `DEVSTACK_DOCKER_TESTS=1` | `crates/devstack-compose/tests/docker.rs` against a real engine | Docker with Linux containers |
| `DEVSTACK_INTEGRATION_NODE=1` with `DEVSTACK_LANE_BIN=<static build>` | `crates/devstack/tests/remote_lane.rs`: a coordinator, a node agent and a lane in Docker | Linux, Docker, a static `x86_64-unknown-linux-musl` build of `devstack` |

The dashboard has its own unit tests (`npm test` in `ui/`, vitest).

## 11. Logs and diagnostics

Replaces what the previous orchestrator's console-log view and its MCP tools
provided, with the build id as the organising key.

- **Capture:** stdout and stderr of every process into a ring buffer per
  process instance (default 10 000 lines) and a file per instance under
  `.devstack/logs/<service>/`, rotated by size, kept for the last N artifacts
  (ADR-0002 retention).
- **Query:** `GET /v1/services/{name}/logs?build=&since=&grep=&tail=`;
  the same parameters in `devstack logs` and the MCP tool. A search over all
  services at once (`/v1/logs?grep=`) answers "where did this error come from".
- **Stream:** log lines on a server-sent event stream per service
  (`/v1/services/{name}/logs/stream`) for the dashboard and `devstack logs -f`.
- **Daemon log:** structured (JSON lines) under `.devstack/daemon.log`, every
  decision with its reason: build superseded, candidate held and why, promotion,
  escalation to kill, port owner found.
- **Not devstack's job:** application traces and metrics. They go to the OTLP
  sink in the downstream Compose file; devstack only passes the build id along
  (§3.3) so traces can be filtered by it.

**Status 2026-10-15:** built differently in places.

- One file per started instance, `.devstack/logs/<service>/run-<ms>-<port>-<build>.log`,
  and one per build, `build-<id>.log`; the newest 20 runs and 10 builds per
  service and 20 runs per command are kept. No ring buffer: the API reads the
  file's tail.
- `devstack logs <service>` and the MCP tool take `tail`, `grep`, `build`
  (the build log and every instance that ran it) and `since` (every instance
  written to within that time); `--list` / `logs/files` shows the kept logs,
  the dashboard as "Earlier logs". A process writes into its file directly,
  so lines have no timestamp of their own: `since` selects whole logs. The
  search over all services is `/v1/logs?grep=` (with `since` as well).
- The daemon log is built as planned: `.devstack/daemon.log`, JSON lines, every
  state change and every request that changed something, with its answer; the
  hub writes `hub.log` next to the token. Both rotate at 5 MB. Read with
  `devstack logs --daemon | --hub` and the MCP tool `diagnostics`; raw process
  output of the daemon goes to `daemon.out`.

## 12. Resource usage

What a stack costs the machine is visible per service, per group and per lane,
because "which lane eats the memory" is the question that comes up as soon as a
second worktree runs.

| Measure | Native processes | Containers |
|---|---|---|
| CPU (% of one core, and total CPU time) | Windows: job accounting of the service's job; Linux: the service's cgroup (`cpu.stat`), else the sum over its process group | `docker stats` of the Compose services |
| Memory (working set / RSS) | Windows: sum over the job's processes; Linux: cgroup `memory.current`, else sum over the process group | `docker stats` |
| Processes | job / cgroup membership (ADR-0009) | — |
| Disk | per lane: snapshot, artifacts, logs under `.devstack/` | named volumes of the Compose project (`docker system df -v`), sampled rarely |

- The daemon samples every 2 s (containers every 5 s) and keeps 10 minutes per
  instance in memory for the dashboard's sparklines; nothing is persisted.
- Builds are accounted separately (the build job, ADR-0020), so a running build
  is visible as such and not as the service's load.
- Aggregation: a service sums its instances (main plus candidates), a group its
  members, a lane its services and its own Compose project if it has one
  (ADR-0030).
- Exposed through `GET /v1/usage`, the SSE stream, `devstack top` and the
  dashboard (§6).

**Status 2026-10-14:** implemented for phase 1 (one lane, no candidates):
`devstack-proc` `Owned::usage` / `BuildJob::usage`, the sampler in
`devstack-daemon` (`usage.rs`), `GET /v1/usage[?history=true]`, a `usage` event
per native sample, `devstack top [--watch]`; the dashboard follows in phase 2.
Known limits of the numbers:

- Windows memory is the summed working set, so pages shared between the tree's
  processes count more than once.
- Linux without a writable cgroup sums CPU time over the live processes; it can
  drop when a process exits, which the sampler reads as "no rate this round".
  Builds on such a host are not measured (they have no common cgroup).
- `docker stats` measures for about two seconds per call; containers lag the
  native numbers by up to five seconds plus that.
- Disk is the logical size of the files (no deduplication, no compression);
  links and junctions are not followed.

## 13. Backlog

What is open, roughly in the order it pays off, and where it comes from.

| Item | From | Size |
|---|---|---|
| GPUs placed: a lane asks for one, the node hands it to the lane's container (`--gpus`), its policy decides (ADR-0036 point 4; reported since 0.9.6) | ADR-0036 | medium |
| Placing by node tags (ADR-0033 point 2: `--on pool:gpu`); the syntax clashes with `<pool>:<node>` (ADR-0034) and is to be decided first | ADR-0033 | small |
| Metrics in the pool's collector: the devstack tenant takes logs only; durations and gauges need a metrics pipeline with TSDB templates and a retention | ADR-0037 | small |
| Watcher on Windows: a folder that arrives by a move (into the tree, or from one folder to another) is reported as itself only; report its files by listing it once the tree has been quiet for a moment, never in the watcher callback (the immediate listing raced `pnpm install`) | the EBUSY fix in `watch.rs` | small |
| A lane's first sync on a node reads every file to hash it (35 s for 4669 files and nothing to send, seen in the pool's telemetry): keep the hashes per worktree on disk (path, size, modification time) and hash in parallel; a restart's carried lane (ADR-0052) syncs in full once for the same reason. `remote_lane_synced` now says where the time goes (`hashed`, `hash_ms`, `compare_ms`, `upload_ms`, `apply_ms`) | pool telemetry 2026-10-22 | small |
| Tunnels with an idle timeout once one side closed, and the open descriptors and tunnels of the node agent and a lane's daemon as gauges (a lane's daemon on a node ran out of descriptors at its soft limit of 1024; the limit is raised to the hard one since) | pool telemetry 2026-10-22 | small |
| A lane on a node sends no telemetry of its own (its services, builds, health): the node could pass the lane daemon's diagnostic lines on (ADR-0037 point 3) | pool telemetry 2026-10-22 | medium |
| Own domain for the landing page (now on GitHub Pages) | phase 4 | small |
| Code signing of the Windows binary (deferred) | [#3](https://github.com/nuetzliches/devstack/issues/3) | small |
| Remove the downstream stack's old orchestrator once its team works without it | phase 4 | downstream |
| `devstack init` checked against a second real repository per language (Go, Python, Spring Boot are detected; .NET and Node came from the reference stack) | §13 | small |
| `devstack migrate`, once devstack has outside users | [ADR-0023](adr/0023-stack-file-compatibility.md) | medium |
| A pull request as a lane, by its number: fetched by refspec, no forge API (`refs/pull/N/head` on GitHub and Forgejo/Gitea, `refs/merge-requests/N/head` on GitLab), the forge told by the remote's URL. A feature that needs the API after all takes its token from `git credential fill` (else `gh`, `glab`, `tea`), never from an OAuth flow of devstack's own: a self-hosted instance has no client ID devstack could ship | [ADR-0047](adr/0047-the-dashboard-makes-a-lane-from-a-branch.md) (what it does not say) | medium |
| Dead ends link onward, once devstack has outside users: a gap left on purpose names its tracking issue (one per gap, so interest adds up instead of duplicates); an open list (forges, programs to open a tree in) links to a prefilled issue form with the version and the OS only, never hosts or paths. One mechanism for it, such as a notice, not links in every error | [ADR-0023](adr/0023-stack-file-compatibility.md) (the same trigger), [ADR-0050](adr/0050-what-no-request-reports-reaches-the-dashboard-as-a-notice.md) | small |
| Network egress sandbox, stage 1 (cooperative proxy) | [ADR-0022](adr/0022-network-egress-sandbox.md) (proposed) | medium |
| Confine file writes of services to the repository (deferred) | [#2](https://github.com/nuetzliches/devstack/issues/2) | medium |
