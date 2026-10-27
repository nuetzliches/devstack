# ADR-0001: Development is ADR-driven

- **Status:** Superseded by [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md) (2026-10-27)
- **Date:** 2026-10-14
- **Related:** [README](README.md), [plan](../plan.md)

## Context

devstack replaces an orchestrator whose behaviour was shaped by dozens of
undocumented workarounds; the reasons lived in commit messages and a runbook,
and every new contributor (human or agent) re-derived them. The project is
intended to be developed largely with AI coding agents, which follow written
constraints well and reconstruct unwritten ones badly.

## Decision

An architectural decision is recorded as an ADR with status `Proposed` before
code depends on it, and moves to `Accepted` before that code is merged. Open
questions are recorded as `Open` ADRs; nothing is built on either side of an
`Open` ADR. `AGENTS.md` points agents at the index.

## Alternatives considered

- **ADRs only for the rare standing constraint** (the convention in some sibling
  projects, where an ADR is the exception). Rejected for this project's early
  phase: the architecture is being decided now, and every major choice is
  contested.
- **A single design document.** Kept as `docs/plan.md` for the overview, but it
  is edited continuously and loses the record of what was rejected.

## Consequences

- More documents early. Admission test in the README keeps small decisions out.
- A pull request introducing a new architectural element without an ADR is
  incomplete.

## Enforced by

- `docs/adr/README.md` (status values, admission test)
- `AGENTS.md` (core expectation 3)
