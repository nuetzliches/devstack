# ADR-0008: Background workers run only in the active slot

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0003](0003-changes-flow-through-candidates.md), [ADR-0012](0012-shared-infrastructure-between-lanes.md)

## Context

Application hosts often run background workers in-process: a job runner polling
a scheduler, a consumer pulling from a queue. When the reference stack launched
duplicate API instances, the losers of the port race had already started their
workers: duplicate job claims, queue messages split between consumers. The
candidate model ([ADR-0003](0003-changes-flow-through-candidates.md)) makes two
instances of a service the normal case for a short time.

## Decision

The stack file declares, per service, the environment that switches its workers
on or off (`workers.env`, with `{is_main}` substituted as `true`/`false`).
devstack starts candidates and every process outside the main slot with workers
off, and the main slot with workers on. A promotion never has two instances with
workers on: the old main stops (gracefully) before the new one starts its
workers, regardless of the mechanism chosen in
[ADR-0010](0010-promotion-mechanism.md).

## Alternatives considered

- **Let applications detect duplicates themselves** (mutex, leader election).
  Every downstream app reimplements it; the reference stack had to.
- **Run candidates against separate infrastructure.** Correct but heavy; this
  is the per-lane question in ADR-0012.

## Consequences

- Downstream applications need a configuration switch for their workers. Most
  have one for tests anyway.
- A candidate's health check cannot prove that its workers start. Worker
  start-up failures surface only after promotion; rollback covers that case.

## Enforced by

- Supervisor (planned): environment composition per slot.
- Stack file validation (planned): a service with `workers` must declare `env`.
