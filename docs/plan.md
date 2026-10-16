# devstack: plan

- **Status:** Active: phases 0 to 2 done (0.4.0), phase 3 built for one machine (0.7.0; each lane with its own infrastructure since 0.8.0, ADR-0030), phase 4 under way; lanes on other machines proposed (ADR-0025 to 0029; see §7 and the backlog, §13)
- **Date:** 2026-10-14, status updated 2026-10-16
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
- **MCP server:** `devstack mcp` (stdio), a client of the same API. Tools map
  one-to-one onto API endpoints ([ADR-0007](adr/0007-one-http-api-is-the-control-surface.md)).
- **Dashboard:** Vue 3 SPA served by the daemon from the same origin
  ([ADR-0006](adr/0006-dashboard-vue-same-origin.md)).

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
7. **Promotion** per [ADR-0010](adr/0010-promotion-mechanism.md) (open:
   restart on the fixed port vs. switch a reverse proxy).
8. The previous main artifact is kept. `devstack rollback <service>` promotes it
   again without a build. Retention: last N artifacts per service (default 5).

**Status 0.2.0:** steps 1-8 are implemented (`crates/devstack-daemon`:
`watch.rs`, the candidate pipeline and `replace` in `supervisor.rs`, `proxy.rs`),
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

Per-lane resources are declared in the stack file (e.g. a database name
template `app_{lane}` on the shared SQL container). Which infrastructure may be
shared between lanes at all is open and not built in the first phases
([ADR-0012](adr/0012-shared-infrastructure-between-lanes.md)). Until then: a
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
the workers. Not
yet: lanes on other machines ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md) to [ADR-0029](adr/0029-source-sync-and-port-forwarding-for-remote-lanes.md) (proposed)).

### 3.6 Daemon crash, restart and update

The daemon takes the whole application plane down with it and never re-attaches
([ADR-0016](adr/0016-a-daemon-restart-is-a-clean-restart.md)). A restart takes
the OS lock on the state directory, removes what a previous instance may have
left (Linux process groups / cgroups, incomplete builds), checks the Compose
groups without restarting them, and starts every service from the artifact that
was last promoted to main, without building. An update is the same path after a
graceful stop.

## 4. Stack file (schema version 1, draft)

Format: TOML, `devstack.toml` in the repository root, no profiles in version 1
([ADR-0017](adr/0017-stack-file-is-toml.md), provisional). A complete example is
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

CLI and API accept a group wherever they accept a service list
(`devstack restart --group portal`, `devstack logs --group telephony`).

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
| `${VAR}`, `${VAR:-default}` | From the process environment and the group's env files |

Resolution order: `{…}` first, then `${…}`, so a default may contain a placeholder.

### 4.6 Deliberately not in version 1

Profiles and layering of stack files, conditionals and expressions, per-lane
overrides and lane placeholders such as `{lane}` (phase 3, plan §3.5), secrets management of its own (secrets come from env files, which the
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
candidate's console, a build timeline per service, a command palette, the lane switcher. Open (§13): tray.

## 7. Phases

Each phase ends with a usable state. Exit criteria are measured on the first
downstream stack.

| Phase | Content | Exit criterion | Status |
|---|---|---|---|
| **0: Spikes** | (a) process ownership, see §7.1; (b) build cancellation, see §7.2; (c) snapshot sync cost for the reference tree; (d) platform benchmark for [ADR-0011](adr/0011-platform-windows-native-first.md) | Each spike answers its question with numbers, recorded in the relevant ADR | Done (2026-10-14) |
| **1: Run plane + CLI** | Stack file, compose driver, `devstack check`, `devstack env --export`, port preflight, artifact builds on demand (`devstack build api`), main slot, jobs, health, logs, job-object ownership, build-id env, CLI, MCP | Reference stack runs end to end without the previous orchestrator; no file-lock failure possible by construction | Done, released as 0.1.0 (§7.3) |
| **2: Candidate pipeline** | Watch, debounce, supersede/cancel, candidates without workers, promotion gate, rollback, SSE events, first dashboard | A broken edit never replaces main; three concurrent writers produce at most one running build per service | Done, released as 0.2.0 (§7.4) |
| **3: Lanes** | Worktree lanes, per-lane resources, `promote --lane`, static frontend artifacts for e2e | Two worktrees run side by side without touching each other's database or the main slot | Built for one machine (0.5.0, ADR-0024): shared infrastructure under ADR-0012's interim rule, per-lane resources by `{lane}` / `{lane_suffix}`, `kind = "static"` (0.6.0); remote lanes open ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md) to [ADR-0029](adr/0029-source-sync-and-port-forwarding-for-remote-lanes.md) (proposed)) |
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

**Not covered by an automated test:** a machine reboot, a Docker Desktop
restart, the `devstack://` link opened from a browser, an IDE terminal closing.

## 8. Open questions

| # | Question | Where it is tracked |
|---|---|---|
| O-1 | Stack file format: TOML, provisional | [ADR-0017](adr/0017-stack-file-is-toml.md), reviewed at the end of phase 1 |
| O-2 | Promotion mechanism: restart on fixed port vs. reverse proxy | [ADR-0010](adr/0010-promotion-mechanism.md): resolved, proxy (B), measured |
| O-3 | Windows-native vs. WSL | [ADR-0011](adr/0011-platform-windows-native-first.md) |
| O-4 | Which infrastructure may be shared between lanes | [ADR-0012](adr/0012-shared-infrastructure-between-lanes.md), deliberately deferred |
| O-5 | Licence | Resolved: MIT OR Apache-2.0; the repository is public since 2026-10-15 |
| O-6 | Default daemon port | Resolved: the daemon takes a free port and writes it to `.devstack/daemon.json`; the hub has a fixed one (4250, `DEVSTACK_HUB_PORT`) |
| O-7 | Lanes per worktree only, or also per agent session in one tree | Phase 3 |
| O-8 | Development values that are not secret (local passwords, dev API keys): a tracked `dev/.env.defaults` next to the untracked `dev/.env`, or everything through the downstream secret chain | Downstream decision; devstack supports several env files per group either way |
| O-10 | How build servers outlive a single build but die with the daemon (spike (b)) | [ADR-0020](adr/0020-builds-share-one-long-lived-build-job.md), accepted |
| O-9 | Minimum Docker Compose version (overlays rely on `!reset`, `!override`, `required: false`) | Resolved: 2.24.4, checked by `devstack doctor` |

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

## 11. Logs and diagnostics

Replaces what the previous orchestrator's console-log view and its MCP tools
provided, with the build id as the organising key.

- **Capture:** stdout and stderr of every process into a ring buffer per
  process instance (default 10 000 lines) and a file per instance under
  `.devstack/logs/<service>/<build-id>-<slot>.log`, rotated by size, kept for
  the last N artifacts (ADR-0002 retention).
- **Query:** `GET /v1/services/{name}/logs?slot=&build=&since=&grep=&tail=`;
  the same parameters in `devstack logs` and the MCP tool. A search over all
  services at once (`/v1/logs?grep=`) answers "where did this error come from".
- **Stream:** log lines on the SSE stream for the dashboard and `devstack logs -f`.
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
  (ADR-0012).
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
| Lanes on other machines, for 0.9.0: M0 spike (devstackd in a lane container, infra by the node, hosts by service name), M1 coordinator and membership (join, approval, certificates, mutual TLS, `devstack pool`; done), M1b the pool in the dashboard ([ADR-0032](adr/0032-the-pool-is-administered-in-the-dashboard.md): roles, admin API with audit log, the hub calling the coordinator with this machine's certificate, a pool area where admins approve, reject and remove members and change roles), M2 a lane on a node: M2a the node agent (`devstack node agent`: leases the coordinator creates and the node admits, mirror sync by content; done), M2b the lane running there (infrastructure, lane container, devstackd in lane mode, the API through the agent; done, smoke test `devstack node run-lane`), M2c the requester (`devstack up --on`, sync from the worktree, renewal, the lane's API under `/v1/lanes/<name>`; every lane shows where it runs: a badge in the sidebar and the lane header, a column in `devstack lane list`; done), M3 forwarded ports (every service port of the lane on this machine at its lane port, through a tunnel to the lane daemon; done), M3b moving a lane (`devstack lane move <lane> --to <node|local>` and in the dashboard: a cold start at the target, the forwarded ports switch once it is healthy, then the old lease goes; ADR-0025 point 10), M4 lifecycle and the rest of the pool area (lanes per node, pausing a node, ending a lease; joining from the dashboard: a form for the coordinator and its fingerprint for a machine in no pool, and for admins the join command with both, ready to copy; the share switch came early) | [ADR-0025](adr/0025-a-lane-can-run-on-another-node.md) to [ADR-0031](adr/0031-a-coordinator-places-lanes.md) | large |
| Own domain for the landing page (now on GitHub Pages) | phase 4 | small |
| Code signing of the Windows binary (deferred) | [#3](https://github.com/nuetzliches/devstack/issues/3) | small |
| Remove the downstream stack's old orchestrator once its team works without it | phase 4 | downstream |
| `devstack init` checked against a second real repository per language (Go, Python, Spring Boot are detected; .NET and Node came from the reference stack) | §13 | small |
| `devstack migrate`, once devstack has outside users | [ADR-0023](adr/0023-stack-file-compatibility.md) | medium |
| Tray icon with status and notifications (deferred) | §6, ADR-0018, [#1](https://github.com/nuetzliches/devstack/issues/1) | medium |
| Network egress sandbox, stage 1 (cooperative proxy) | [ADR-0022](adr/0022-network-egress-sandbox.md) (proposed) | medium |
| Confine file writes of services to the repository (deferred) | [#2](https://github.com/nuetzliches/devstack/issues/2) | medium |
