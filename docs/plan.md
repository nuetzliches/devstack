# devstack: plan

- **Status:** Draft
- **Date:** 2026-10-14
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
  artifact and serves it from the daemon on its own port. Intended for
  end-to-end runs that must pin a build id.

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
| `compose.<group>` | table | A Compose project (ADR-0004) |
| `services.<name>` | table | A native process managed by devstack |
| `commands.<name>` | table | An operation users and agents can run |

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
| `build` | Command run in the lane's snapshot, with the toolchain's normal output directories (ADR-0002); absent for `devserver`/`tool` |
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
| `compose` | `{ group, service }` whose environment is the base (ADR-0014) |
| `rewrite_hosts` | Compose service names rewritten to published host addresses (ADR-0014) |
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
| `POST /v1/commands/{name}` | Custom commands from the stack file |
| `POST /v1/infra/{service}/restart` | Container restart (e.g. after a config change the container cannot see) |

**Security:** loopback only; every mutating request requires a token stored in
the user profile (readable by the CLI and MCP, embedded by the daemon into the
dashboard it serves) and an `Origin` check, so an arbitrary web page in the same
browser cannot drive the daemon. To be written up as an ADR in phase 1.

## 6. Dashboard scope (phase 2 and later)

Delivery: a browser page served by the daemon, opened with `devstack ui`; status
icon and notifications through a tray in the daemon binary ([ADR-0018](adr/0018-dashboard-is-a-browser-page.md)).

- Stack overview: infra and services, health, slot, build id, ports, lane.
- Build timeline per service, including superseded builds and durations.
- Live logs per process.
- Actions: promote, rollback, restart, custom commands (with confirmation).
- Lane switcher.
- Explicitly not: a telemetry viewer. Traces and metrics go to an OTLP sink
  defined in the downstream compose file.

## 7. Phases

Each phase ends with a usable state. Exit criteria are measured on the first
downstream stack.

| Phase | Content | Exit criterion |
|---|---|---|
| **0: Spikes** | (a) process ownership, see §7.1; (b) build cancellation, see §7.2; (c) snapshot sync cost for the reference tree; (d) platform benchmark for [ADR-0011](adr/0011-platform-windows-native-first.md) | Each spike answers its question with numbers, recorded in the relevant ADR |
| **1: Run plane + CLI** | Stack file, compose driver, `devstack check`, `devstack env --export`, port preflight, artifact builds on demand (`devstack build api`), main slot, jobs, health, logs, job-object ownership, build-id env, CLI, MCP | Reference stack runs end to end without the previous orchestrator; no file-lock failure possible by construction |
| **2: Candidate pipeline** | Watch, debounce, supersede/cancel, candidates without workers, promotion gate, rollback, SSE events, first dashboard | A broken edit never replaces main; three concurrent writers produce at most one running build per service |
| **3: Lanes** | Worktree lanes, per-lane resources, `promote --lane`, static frontend artifacts for e2e | Two worktrees run side by side without touching each other's database or the main slot |
| **4: Adoption** | Migrate the downstream stack, delete its old orchestrator, publish | Downstream runbook shrinks to the stack file and a page of notes |

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
daemon console as the stop method; ConPTY fails to stop node (open). Linux half
open. See `spikes/process-ownership/README.md`.

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

## 8. Open questions

| # | Question | Where it is tracked |
|---|---|---|
| O-1 | Stack file format: TOML, provisional | [ADR-0017](adr/0017-stack-file-is-toml.md), reviewed at the end of phase 1 |
| O-2 | Promotion mechanism: restart on fixed port vs. reverse proxy | [ADR-0010](adr/0010-promotion-mechanism.md) (leaning: proxy) |
| O-3 | Windows-native vs. WSL | [ADR-0011](adr/0011-platform-windows-native-first.md) |
| O-4 | Which infrastructure may be shared between lanes | [ADR-0012](adr/0012-shared-infrastructure-between-lanes.md), deliberately deferred |
| O-5 | Licence (Croniq uses MIT OR Apache-2.0) | Before going public |
| O-6 | Default daemon port | Phase 1 |
| O-7 | Lanes per worktree only, or also per agent session in one tree | Phase 3 |
| O-8 | Development values that are not secret (local passwords, dev API keys): a tracked `dev/.env.defaults` next to the untracked `dev/.env`, or everything through the downstream secret chain | Downstream decision; devstack supports several env files per group either way |
| O-9 | Minimum Docker Compose version (overlays rely on `!reset`, `!override`, `required: false`) | Phase 1, checked by `devstack check` |

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
