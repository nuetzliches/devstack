# ADR-0014: Native services take their configuration from the Compose service they replace

- **Status:** Proposed
- **Date:** 2026-10-14
- **Related:** [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0002](0002-processes-run-from-immutable-artifacts.md)

## Context

A deployed application container carries its configuration in its Compose
service: environment variables, often a hundred or more, addressing other
services by their Compose DNS names (`http://scheduler:4000`). When the same
application runs natively in development, it needs that configuration again,
with addresses pointing to published ports on the host instead.

In the reference stack the development configuration was a second, parallel
description: orchestrator parameters (some of them literal values that silently
override application settings), a settings file generated on every build from a
template, launch-profile variables, and a guard checking all of that. Each layer
existed to correct another, and they drifted from the deployed configuration.

## Decision

A native service can name the Compose service it replaces:

```toml
[services.api]
compose = { group = "app", service = "api" }
rewrite_hosts = ["scheduler", "renderer", "otel-collector"]
```

At start, devstack reads the fully interpolated service from
`docker compose config --format json` (same group, same files, same env file as
the infra plane, profiles enabled so parked services resolve) and builds the
process environment from it:

1. the service's `environment`,
2. host rewriting: for every name listed in `rewrite_hosts`, occurrences of
   `<name>:<container port>` in values become `127.0.0.1:<published host port>`.
   A listed name without a published port is a start error, not a silent
   pass-through,
3. the stack file's own `env` for the service, which wins over both,
4. devstack's variables (`DEVSTACK_BUILD_ID`, `DEVSTACK_LANE`, `DEVSTACK_SLOT`,
   worker switches from [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md)).

What differs between development and deployment is expressed the way it is for
every other environment: in the development env file. `devstack env <service>`
prints the resolved environment with the origin of each variable.

**The rule is a documented function, not a devstack-only mechanism**
([ADR-0015](0015-devstack-is-optional.md)). Steps 1–3 depend only on Compose and
the stack file; any script can reproduce them. `devstack env <service> --export
dotenv [--out <file>]` writes the result (steps 1–3, without devstack's own
variables) to a file, so a service can be started by hand or from an IDE launch
profile without a running daemon. Exported files contain development secrets
and belong in git-ignored locations.

## Alternatives considered

- **Configuration in the stack file.** A third copy of the same settings.
- **Automatic rewriting of every Compose service name found in any value.**
  Less to write, but it rewrites strings that merely look like hosts, and a
  forgotten published port fails at runtime instead of at start. Rejected in
  favour of the explicit list.
- **Run the application as a container in development too.** Removes the
  question, but loses fast native builds, native debugging and the candidate
  model of ADR-0002/0003.

## Consequences

- Development configuration and deployment configuration are one artifact; a
  deployment change reaches development on the next start.
- Values that make sense only inside a container (container paths, mounted
  secrets files) need an override in the development env file or the stack file.
- Native processes depend on `docker compose config` working, even for services
  that are parked.

## Enforced by

- Process environment builder (planned).
- `devstack env <service>` (planned) for inspection, `--export dotenv` for use
  without devstack.
- `devstack check` (planned): every native service's environment must be
  exportable (ADR-0015).
