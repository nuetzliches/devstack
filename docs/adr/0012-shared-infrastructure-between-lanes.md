# ADR-0012: Which infrastructure may be shared between lanes

- **Status:** Open. The interim rule below is what lanes do since 0.5.0
  ([ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md)); the
  question stays open for per-lane coordinators.
- **Date:** 2026-10-14
- **Related:** plan §3.5, [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md)

## Context

Lanes (a worktree, possibly an agent session) run their own candidates. Running
a complete infra plane per lane is expensive (memory, start-up, disk); sharing
it is cheap but not always correct.

Two kinds of infrastructure behave very differently when shared:

- **Passive stores** (a SQL server, a search engine, object storage) can be
  shared safely if each lane uses its own database, index prefix or bucket.
- **Coordinators that dispatch work** (job schedulers, pull queues, webhook
  ingress with consumers) cannot. A shared scheduler hands a job to whichever
  lane's runner polls first, so a job runs against the wrong lane's code and
  database. Its job definitions also come from one lane's working tree, not the
  other's. A shared pull queue splits events between lanes.

## Interim rule (until this ADR is decided)

Non-main lanes run without workers (ADR-0008) and therefore do not talk to
dispatching coordinators at all. Passive stores are reached through per-lane
resource names declared in the stack file (e.g. a database name template).

## Question

Should sharing be configurable per infra service, and if so, with which
safeguards?

## Options

- **A. Never share; one infra plane per lane** (Compose project per lane, port
  offsets). Simple and correct, heavy.
- **B. Share passive stores, isolate coordinators per lane** (a scheduler
  container per lane, small). The stack file marks each infra service
  `share = "lane-scoped-resource" | "per-lane-instance"`.
- **C. Fully configurable**, including sharing a coordinator, with a validation
  error unless the service declares how lanes are separated (e.g. a lane tag
  that runners filter on).

## What would decide it

Experience from phase 3 with the interim rule: whether candidates in non-main
lanes need their workers at all for typical tasks, and the memory cost of a
per-lane coordinator on a developer machine.
