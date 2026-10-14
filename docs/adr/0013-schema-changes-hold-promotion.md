# ADR-0013: A build that changes schema inputs is not promoted automatically

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0003](0003-changes-flow-through-candidates.md)

## Context

Most changes are reversible by promoting the previous artifact. Schema
migrations are not: once a migration job ran against the shared development
database, rolling back the code leaves a database ahead of it. With automatic
promotion, a half-written migration saved by one agent could be applied while
another agent still works against the old schema.

## Decision

A service may declare `promote.hold_if_changed = [globs]`. If a candidate's
input differs from main's in any matching file, the candidate is built and
health-checked (against the current schema, with its migration job not run) and
then **held** until `devstack promote <service>`. Promotion then runs the
declared migration job before switching. The dashboard and status output show
held candidates prominently.

## Alternatives considered

- **Always auto-promote.** See context.
- **Run the candidate against a cloned database.** Correct and fully automatic,
  but cloning a realistic database on every schema edit is slow; may return as
  an option once lanes have per-lane databases (ADR-0012).
- **Never auto-promote anything.** Turns the common case into a manual step.

## Consequences

- A held candidate blocks later auto-promotions of the same service until it is
  promoted or discarded; otherwise main would skip the migration.
- The health check of a held candidate may fail because the new code expects
  the new schema; that is reported as "held, health unknown", not as a failure.

## Enforced by

- Promotion gate in the supervisor (planned); stack file validation (planned).
