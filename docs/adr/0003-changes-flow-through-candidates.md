# ADR-0003: Changes flow through candidates; the main slot only holds a stack that built and passed health

- **Status:** Proposed
- **Date:** 2026-10-14
- **Related:** [problem statement, root cause B](../problem-statement.md#root-cause-b-the-watcher-restarts-on-every-change-regardless-of-outcome), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0010](0010-promotion-mechanism.md)

## Context

A watcher that restarts the running service on every change replaces a working
stack with whatever the tree contains at that second: half an edit, a file one
agent saved while another is mid-refactor. With several concurrent writers the
reference stack launched the API three times within one second.

## Decision

Every service has a **main slot** (fixed ports, workers enabled) and zero or
more **candidates**. A change is debounced, then built into an artifact; a newer
change for the same lane and service **cancels** the running build (whole
process tree) and starts over. A successful build starts as a candidate on a
free port without workers; if it passes its health check it is **promoted** to
main, automatically unless a promotion gate holds it
([ADR-0013](0013-schema-changes-hold-promotion.md)) or the user has paused
auto-promotion. A failed build or failed health check leaves main untouched and
is reported.

**Verified by spike (b), 2026-10-14** (`spikes/build-cancel`): 14 builds of the
reference API host killed at random moments (restore, compilation, copy); every
following build succeeded with byte-identical assemblies and was at most 6 %
slower than an uninterrupted one, often much faster. A hard kill takes 2–10 ms;
a graceful `CTRL_BREAK` takes up to 14 s and buys nothing. **Superseded builds
are therefore killed, not asked to stop.** The same holds for the Angular
production build.

## Alternatives considered

- **Restart on change (status quo of most tools).** See context.
- **Queue builds instead of cancelling them.** Every intermediate state gets
  built and possibly promoted; with frequent saves the queue only grows.
- **Build on change, promote only on explicit command.** Safe, but turns the
  common case into a manual step. Available as a per-service setting
  (`promote.auto = false`), not the default.

## Consequences

- Latency from save to running code grows by the candidate health check.
- Two instances of a service exist briefly; services must tolerate a second
  instance without workers ([ADR-0008](0008-background-workers-run-only-in-the-active-slot.md)).
- Build history gains a `superseded` state, distinct from `failed`.

## Enforced by

- Builder (planned): one build slot per (lane, service), cancellation on supersede.
- Supervisor (planned): promotion is the only path into main.
