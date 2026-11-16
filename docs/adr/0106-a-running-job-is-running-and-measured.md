# ADR-0106: A running job is `running` and measured; it counts as done only when it completed

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-16
- **Deciders:** project owner
- **Related:** [ADR-0059](0059-a-services-stop-command-ends-what-its-run-started.md) (a job's `stop`), issue #544, pull request #547

## Context

A `kind = "job"` (a migration, a seed, a training run of hours in a lane on
a node) showed `starting` for its whole run, with no CPU or memory: its
instance never became the slot's `main` (`start_locked` waited for it in
`wait_instance` as a local and ended it there), so `tree_usage` had nothing
to measure, and `starting` for hours read as stuck. A service waiting for
its health check was unmeasured the same way, for a shorter while.

## Decision

The owner approved the review card in the conversation on **2026-11-16**,
points 1 and 2:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A job's state is `running` from the launch of its process until it ends (was `starting`), in the API, the events, `status` (CLI, MCP) and the dashboard. | A new state (`working`) every client would have to learn | Clients that read a job's state; the dashboard's mock showed `running` already |
| 2 | contract | `devstack up` and a command's `requires` count a job as done only at `completed`, never at `running`. | `running` as ready, as for other services | Low |

As built:

1. **Usage** (`supervisor/mod.rs`, `lifecycle.rs`): a slot keeps what the
   instance being started uses (`starting_usage`), sampled by
   `wait_instance` every `NATIVE_EVERY` (as the usage sampler asks) while a
   job runs or a service waits for its health check; `tree_usage` takes
   `main`, else that, and the start clears it when it is over. A
   candidate's wait notes nothing: its service's main is measured.
2. **State:** `start_locked` sets a job `running` right after `starting`.
   `set_status_of` keeps the start's time for a job's `running`, so the
   start's duration in the diagnostic log is still its run.
3. **Done:** dependants (`after`) waited for `completed` already; a
   command's `requires` (`commands.rs`) and `up`'s follow (`client.rs`)
   now do the same for a job.

## Alternatives considered

- **A phase inside `starting`** (`{ state: "starting", phase: "running" }`):
  every client keeps reading `starting`, the point of the issue.
- **The node's measurement of a lease's containers** for a lane on a node:
  it would show a lane's load, not which service causes it; left for later.

## Consequences

- A script that waited for a job's `starting` waits for `running` now
  (three integration tests did).
- A job that runs is green ("Running") in the dashboard, like a service
  without a health check.

## Enforced by

- `crates/devstack-daemon/src/supervisor/lifecycle.rs` (`start_locked`,
  `wait_instance`), `supervisor/mod.rs` (`Slot::starting_usage`,
  `tree_usage`, `set_status_of`), `state.rs` (`Status::Running`)
- `crates/devstack-daemon/src/commands.rs` (`requires`),
  `crates/devstack/src/client.rs` (`up`'s follow)
- `crates/devstack/tests/daemon_lifecycle.rs` (`a_hanging_job_or_step_ends`
  measures the running job), `supervisor/lifecycle.rs` tests (Windows)
- `docs/dashboard-api.md`

## What this ADR does not say

- That a job's usage history survives its end: it is gone with the job, as
  a stopped service's.
