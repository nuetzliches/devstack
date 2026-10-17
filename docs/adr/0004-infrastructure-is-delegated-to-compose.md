# ADR-0004: Infrastructure comes from the project's existing Compose files plus a dev overlay

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [problem statement, root cause C](../problem-statement.md#root-cause-c-an-opaque-orchestration-layer-between-host-and-services), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md)

## Context

In the reference stack the orchestrator owned container identity (names derived
from a path hash), ports (an endpoint proxy in front of every container) and
lifecycle. Its proxy wedged without any visible cause; a second worktree's stack
silently talked to the first one's containers. It also described every
container a second time, in its own API, next to the Compose files the project
already maintains for staging and production. The two descriptions drifted.

Projects that deploy with Compose typically have **several** Compose projects
(one per host or concern, sometimes joined by external networks) and one
`.env` per environment. Development is, from Compose's point of view, just
another environment.

## Decision

devstack does not create, network or proxy containers, and it does not ask for a
separate description of them. The infra plane is one or more **Compose groups**
declared in the stack file. Each group is a Compose project with

- a fixed project name (no path-derived names),
- a stack of files: the project's **existing** deployment Compose file(s) first,
  then a **dev overlay** that contains only the differences,
- an env file for the development environment,
- an explicit selection of services to start.

devstack runs `docker compose -p <name> -f … --env-file … up -d --wait --no-deps
<selected services>`, creates declared external networks beforehand, reads state
from `docker compose ps --format json` and orders groups and services itself.
`--no-deps` is deliberate: dependencies are started by devstack in declared
order, never pulled in implicitly.

**What a dev overlay typically contains:**

1. **Parks** the application containers that devstack runs natively instead,
   by moving them into a profile (`profiles: [container-app]`). They can still be
   started as containers by enabling the profile (end-to-end runs, lanes).
2. **Publishes** ports on `127.0.0.1` for services native processes must reach.
3. **Rewires** containers that call an application service to
   `host.docker.internal` (plus `extra_hosts: ["host.docker.internal:host-gateway"]`
   for Linux engines).
4. **Replaces** production-only services with development counterparts (e.g. a
   telemetry dashboard instead of a telemetry store, a local database instead of
   an external one).
5. **Pulls in** services defined in other hosts' Compose files with
   `extends: { file, service }` instead of copying them.

Mock servers whose code lives in the repository are **native services** in the
stack file, not containers; only image-only mocks belong in the overlay.

**Two Compose behaviours the overlay has to handle** (verified with Compose
v5.5.1):

- Parking a service behind a profile makes every hard `depends_on` on it fail
  the whole configuration (`depends on undefined service`). The overlay marks
  such dependencies `required: false` (keeps the ordering when the container mode
  is enabled) or removes them with `!reset`.
- `extends` carries `depends_on` and network *references* over, but not the
  services and networks they refer to. The overlay declares them or resets the
  references (`depends_on: !reset []`, `networks: !override [...]`).

Config files that a container must see change are not expected to propagate
over a bind mount from the Windows file system: change notifications do not
cross that boundary. A group service can declare `restart_on = [paths]`, and
devstack restarts the container when they change.

## Alternatives considered

- **Own container management via the Docker API.** Duplicates Compose and
  repeats the reference orchestrator's mistake of owning everything.
- **A separate, complete dev Compose file** (the first draft of this ADR). One
  file to read, but a second description of every service, drifting from the
  deployed one exactly like the orchestrator's model did.
- **Compose `include:`** to assemble hosts. Rejected: it imports whole projects
  and fails on duplicate service names (every host has its own reverse proxy),
  while `extends` takes exactly one service.
- **Testcontainers-style ephemeral containers per start.** Loses data between
  starts and slows every start.

## Consequences

- Development uses the same images, versions, health checks and configuration
  as deployment; the overlay is the visible list of differences.
- A change to a deployment Compose file can break development. Downstream
  projects should validate the dev stack (`docker compose … config -q`) in the
  same CI check that validates their deployment files.
- Overlays depend on Compose merge semantics (`!reset`, `!override`, additive
  port lists), so a minimum Compose version is part of devstack's requirements.
- Per-lane infrastructure needs a second project name or profiles; see
  [ADR-0012](0012-shared-infrastructure-between-lanes.md).

## Enforced by

- Infra driver (planned): only shells out to `docker compose`; always passes
  `-p` and `--no-deps`.
- Stack file validation (planned): every group names its project and files;
  `devstack check` runs `docker compose config -q` for every group.

*Paths as built, noted 2026-10-18.* `crates/devstack-compose/src/lib.rs`
(`Group::base_args`: `-p <project>` on every call; `Docker::up` with
`--no-deps` and the group's services; a time budget on every Docker call)
and `engine.rs` (Docker Desktop and the engine's state);
`crates/devstack-config/src/validate.rs` (every group names a project and
its files, inside the repository); `devstack check --compose`
(`crates/devstack/src/main.rs`, `Docker::validate`) runs `docker compose
config -q` per group, plain `devstack check` does not call Docker. Tests:
the unit tests of `devstack-compose` and `crates/devstack-compose/tests/docker.rs`.
