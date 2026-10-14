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
4. **Build** in the snapshot directory into
   `.devstack/artifacts/<service>/<build-id>/`. If a newer change for the same
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

## 4. Stack file (sketch, not final)

Format and name are not decided (open question O-1). Illustration only:

```toml
[stack]
name = "example"

# Infra plane: the project's own deployment Compose files plus a dev overlay
# (ADR-0004). One group per Compose project; order = start order.
[[compose]]
name     = "search"                                   # project name, fixed
files    = ["deploy/app-host/compose.search.yml", "dev/search.dev.yml"]
env_file = "dev/.env"
networks = ["edge"]                                   # external, created before `up`
services = ["search"]

[[compose]]
name     = "app"
files    = ["deploy/app-host/compose.yml", "dev/app-host.dev.yml"]
env_file = "dev/.env"
services = ["db", "scheduler", "renderer", "otel-collector", "dashboard", "webhooks"]
restart_on = { scheduler = ["config/scheduler/**"] }  # bind mounts do not notify

[profiles]
local  = "dev/profile.local.toml"
shared = "dev/profile.shared.toml"

[services.migrate]
kind    = "job"
compose = { group = "app", service = "migrate" }      # environment from Compose (ADR-0014)
rewrite_hosts = ["db"]
watch   = ["src/**/*.cs", "src/**/*.csproj"]
build   = "dotnet build src/Migrate -c Debug -o {artifact}"
run     = "{artifact}/Migrate.exe"

[services.api]
compose = { group = "app", service = "api" }
rewrite_hosts = ["db", "scheduler", "renderer", "otel-collector"]
watch   = ["src/**/*.cs", "src/**/*.csproj", "src/**/appsettings*.json"]
build   = "dotnet build src/Api -c Debug -o {artifact}"
run     = "{artifact}/Api.exe"
port    = 5000
after   = ["migrate"]
health  = { http = "/health", timeout = "60s" }
workers = { env = { Jobs__Enabled = "{is_main}", Queue__Consumer__Enabled = "{is_main}" } }
promote = { hold_if_changed = ["src/Data/Migrations/**"] }

[services.payment-mock]                               # mock with code in the repo: native
watch = ["tools/payment-mock/**"]
build = "dotnet build tools/payment-mock -c Debug -o {artifact}"
run   = "{artifact}/PaymentMock.exe"
port  = 5310
health = { http = "/health", timeout = "30s" }

[services.web]
kind   = "devserver"
cwd    = "web"
run    = "npx ng serve --port {port} --ssl"
port   = 4200
health = { http = "/", timeout = "180s", insecure_tls = true }
restart_on = ["web/tsconfig*.json", "web/angular.json"]

[commands.reset-db]
description = "Drop and rebuild the local database"
confirm     = true
steps       = ["stop api", "exec dev/reset-db.sh", "run migrate", "start api"]
```

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
| **0: Spikes** | (a) Windows Job Objects + graceful stop from Rust, incl. `dotnet` and `node` trees; (b) cancelling an MSBuild tree mid-build and resuming incrementally; (c) snapshot sync cost for the reference tree; (d) platform benchmark for [ADR-0011](adr/0011-platform-windows-native-first.md) | Each spike answers its question with numbers, recorded in the relevant ADR |
| **1: Run plane + CLI** | Stack file, compose driver, `devstack check`, `devstack env --export`, port preflight, artifact builds on demand (`devstack build api`), main slot, jobs, health, logs, job-object ownership, build-id env, CLI, MCP | Reference stack runs end to end without the previous orchestrator; no file-lock failure possible by construction |
| **2: Candidate pipeline** | Watch, debounce, supersede/cancel, candidates without workers, promotion gate, rollback, SSE events, first dashboard | A broken edit never replaces main; three concurrent writers produce at most one running build per service |
| **3: Lanes** | Worktree lanes, per-lane resources, `promote --lane`, static frontend artifacts for e2e | Two worktrees run side by side without touching each other's database or the main slot |
| **4: Adoption** | Migrate the downstream stack, delete its old orchestrator, publish | Downstream runbook shrinks to the stack file and a page of notes |

## 8. Open questions

| # | Question | Where it is tracked |
|---|---|---|
| O-1 | Stack file format and name (TOML, YAML, own DSL like Croniqfile) | Becomes an ADR in phase 1 |
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
