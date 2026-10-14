# Problem statement

This document records why devstack exists: the failure modes observed in a real
local development stack, reduced to their root causes, and the requirements
that follow. It is written generically. The downstream stack it was derived
from is described only as far as needed to judge the argument.

## The reference stack

- A .NET API host plus a schema migrator that must finish before the API starts.
- A handful of smaller .NET processes (mock servers for third-party APIs, an
  auxiliary portal API, a Windows-only desktop helper).
- An Angular SPA served by the Angular dev server over HTTPS.
- Around twenty containers: SQL Server, Elasticsearch, a job scheduler, a
  webhook ingress with a pull queue, an OpenTelemetry collector plus dashboard,
  an uptime checker, SMB, a virus scanner, a PDF renderer, PostgreSQL, S3
  storage, a local LLM stack.
- Development on Windows 11, containers via Docker Desktop (WSL2 backend).
- Several launch profiles (local throw-away database vs. shared development
  database).
- **Several writers in one working tree at the same time:** an IDE, terminals
  and multiple AI coding agents. This is the normal case, not the exception.

It was orchestrated with a .NET Aspire AppHost started under `dotnet watch`.
Over three months the AppHost, its launcher script and the runbook around it
grew to roughly 3,300 lines, a large share of them workarounds.

## Observed failure modes

Grouped by root cause. Every item was observed at least once and cost real time.

### Root cause A: building and running share one output directory

| Symptom | Mechanism |
|---|---|
| A new endpoint answers 404 although the code is there and the project builds. | The running process locks its own DLLs. The rebuild fails with "file in use", the output stays partly old, nobody reports it. |
| A new field is silently ignored, no exception anywhere. | Same: the old assembly is still loaded. |
| "Restart" does not pick up the change. | Restart starts the process again; it does not build. |
| "Rebuild" fails as well. | It builds into the directory the still-living process holds. |
| The orchestrator reports a resource as stopped while the process lives on. | Stop signal acknowledged, process tree not actually gone. |
| A zombie process (0 threads, no path) locks the output directory and cannot be killed even as administrator. | Leftover of a parallel launch. Only renaming the directory helps. |
| The SPA dev server keeps serving a bundle that is minutes old; a new path alias needs a full restart. | Watcher wedged during edits; no way to see which bundle is live. |
| Restoring a source file with `cp`/`mv` does not trigger a rebuild. | mtime of the restored file predates the output. |

**Root cause:** the artifact that runs and the artifact being built are the same
files. There is no build identity, so "running" and "running my change" look
identical.

### Root cause B: the watcher restarts on every change, regardless of outcome

| Symptom | Mechanism |
|---|---|
| The API is launched three times within one second. | `dotnet watch` on the orchestrator watches the whole project graph; several agents save files concurrently. Duplicate instances start background workers before losing the port race, double-consuming queues and job claims. A single-instance mutex had to be added to the application itself. |
| A half-finished edit takes down a stack that was working a moment ago. | Restart happens before anyone knows whether the new state compiles. |

**Root cause:** there is no notion of a *candidate*. Every change replaces the
running stack, successful or not, and changes are not coalesced or cancelled.

### Root cause C: an opaque orchestration layer between host and services

| Symptom | Mechanism |
|---|---|
| Resource "unhealthy" although the container answers on its real port in 2 ms. | The orchestrator's endpoint proxy accepts connections but stops relaying. Only a full orchestrator restart helps; per-resource restart does not. |
| Same symptom, different cause: a foreign desktop application owned the pinned port. | Indistinguishable from the proxy wedge without checking the port owner by hand. |
| One unhealthy dependency blocks half the stack indefinitely. | Dependency waits have no timeout and no reason. |
| A stack started from a second worktree writes migrations into the first worktree's database. | Persistent containers are named by a hash of the project path; the old ones keep the fixed ports and serve the new stack. |
| Stopping the stack needs a hand-written Ctrl+C injector. | A hard kill leaves claimed jobs, unacknowledged queue messages and the proxy wedge behind. |
| Configuration has five layers (launch profiles, literal parameters that override app settings, generated settings files, a branch-tracked version file, a guard). | Each layer exists to work around another. |

**Root cause:** the orchestrator owns ports, proxies, container identity and
lifecycle at once, and its state is not inspectable.

### Independent of the orchestrator

- File-change notifications do not cross Docker bind mounts from NTFS into Linux
  containers. A container's config watcher logs "watching for changes" and never
  fires; every config change needs a container restart.
- Two writers sharing one coordinator are a correctness bug, not a resource
  question: a scheduler or pull queue shared by two stacks hands work to
  whichever stack asks first.

## Requirements

### Must

1. **Infrastructure is long-lived** and independent of application changes;
   containers are health-gated with timeouts and named failure reasons.
2. **No second description of the stack.** Development reuses the project's
   existing deployment Compose files; differences live in an overlay. Native
   processes take their configuration from the Compose service they replace.
3. **A main slot on fixed ports** that only changes to a stack that built and
   passed health checks. A broken build never replaces a working stack.
4. **Build identity is observable:** every running process and served bundle
   reports which input it was built from.
5. **Building never touches what is running.**
6. **Changes are coalesced and superseded:** a newer change cancels the build of
   an older one.
7. **Ordered startup** including run-to-completion jobs (migrations) before
   dependants, with timeouts.
8. **Profiles and secrets** without layered overrides: one declarative stack
   file plus profile overlays plus an untracked secrets file.
9. **Clean shutdown:** graceful stop first, then a guaranteed kill of the whole
   process tree. No orphans, ever, including when the daemon itself dies.
10. **One control surface** usable by people (CLI, dashboard) and agents (MCP)
   with identical semantics: status, logs, build history, promote, rollback,
   restart, custom commands (e.g. "reset database").
11. **Telemetry sink** for OTLP traces, logs and metrics.
12. **Windows-native operation** (see ADR-0011).

### Should

- Lanes: a worktree or agent session can build and run its own candidate
  without disturbing the main slot.
- End-to-end tests can run against an immutable build instead of a moving dev
  server.
- Port ownership is checked before start and reported by process name.
- One command brings the stack up; one screen shows its state.

### Non-goals

- Production deployment. devstack is for one developer machine.
- Replacing Docker Compose or Kubernetes.
- Language plugins beyond what the first downstream stack needs (.NET, Node).
  The stack file stays command-based, so other toolchains work without plugins.
