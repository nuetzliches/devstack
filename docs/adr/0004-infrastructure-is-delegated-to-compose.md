# ADR-0004: Infrastructure is delegated to Docker Compose

- **Status:** Proposed
- **Date:** 2026-10-14
- **Related:** [problem statement, root cause C](../problem-statement.md#root-cause-c-an-opaque-orchestration-layer-between-host-and-services)

## Context

In the reference stack the orchestrator owned container identity (names derived
from a path hash), ports (an endpoint proxy in front of every container) and
lifecycle. Its proxy wedged without any visible cause; a second worktree's stack
silently talked to the first one's containers. Downstream projects usually
already maintain a Compose file for staging or production with the same images.

## Decision

devstack does not create, network or proxy containers. The infra plane is a
Compose file referenced by the stack file, under a **fixed project name**.
devstack runs `docker compose up -d --wait`, reads state and health from
`docker compose ps --format json`, and restarts individual services on request.
Container ports are published directly; health checks live in the Compose file.

Config files that a container must see change are not bind-mounted from the
Windows file system with the expectation that the container notices: file
change notifications do not cross that boundary. The stack file can declare
`restart_on = [paths]` per infra service, and devstack restarts the container
when they change.

## Alternatives considered

- **Own container management via the Docker API.** Duplicates Compose and
  repeats the reference orchestrator's mistake of owning everything.
- **Testcontainers-style ephemeral containers per start.** Loses data between
  starts and slows every start.

## Consequences

- Downstream projects maintain a dev Compose file; ideally derived from their
  production one, which reduces drift.
- Per-lane infrastructure needs Compose profiles or a second project name; see
  [ADR-0012](0012-shared-infrastructure-between-lanes.md).

## Enforced by

- Infra driver (planned): only shells out to `docker compose`.
