# ADR-0015: devstack accelerates a stack, it is never required to run one

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md)

## Context

The orchestrator devstack replaces was a lock-in: its stack model was code that
only it could execute. Without it there was no development stack at all, only a
deployment description for other hosts. Moving away from it meant rebuilding
the stack description from scratch.

Downstream teams adopting devstack want the opposite guarantee: if devstack is
abandoned, broken, or simply not installed on a machine, the development stack
still runs, slower and with less comfort, but completely.

## Decision

Every stack devstack runs can also be run without it, with Docker Compose and
the commands written in the stack file. Concretely:

1. **Infrastructure** is plain Compose (ADR-0004): the deployment files plus a
   dev overlay, started with `docker compose`.
2. **The whole stack as containers** is always possible: application services
   parked by the overlay start with `--profile container-app`.
3. **Native services** are started by commands any developer can run by hand
   (`dotnet build`, `npm`, `ng serve`). Their environment follows a documented,
   tool-independent rule (ADR-0014) and can be exported with
   `devstack env <service> --export dotenv`, so IDE and debugger launches work
   without a running daemon.
4. **Applications never depend on devstack.** No devstack library, no required
   devstack variable. Worker switches (ADR-0008) are ordinary configuration
   keys; `DEVSTACK_*` variables are informational and optional.

devstack adds what Compose and hand-run commands cannot: immutable artifacts,
candidates and promotion, lanes, process-tree ownership, one control surface.
Losing devstack loses those, not the stack.

## Alternatives considered

- **devstack as the only way to run the stack.** Simpler documentation, and
  exactly the lock-in this project exists to remove.
- **Generate a standalone launcher (scripts, Procfile) from the stack file.**
  Generated artifacts drift and become a second description; the stack file
  already holds the commands in readable form.

## Consequences

- Features that would only work through devstack (e.g. configuration available
  nowhere but inside the daemon) are rejected or need an export path.
- Downstream projects document the Compose-only path in one place and keep it
  working; it doubles as the fallback when devstack misbehaves.

## Enforced by

- `devstack check` (planned): fails when a native service's environment cannot
  be exported, when a Compose group does not validate on its own
  (`docker compose config -q`), or when the `container-app` profile is missing
  for a parked service.
- ADR-0014 (export path), ADR-0008 (plain configuration keys).

*Paths as built, noted 2026-10-18.* `devstack check --compose`
(`crates/devstack/src/main.rs`, `Docker::validate` in
`crates/devstack-compose`) fails when a Compose group does not validate on
its own. The export path is `devstack env <service> --export dotenv`
(`crates/devstack/src/envcmd.rs`, `to_dotenv` in `crates/devstack-env`,
Compose's dialect, tested to read back exactly). Not built: failing `check`
when an environment cannot be exported, and the check for a missing
`container-app` profile.
