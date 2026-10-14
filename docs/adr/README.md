# Architecture Decision Records

devstack is developed ADR-driven ([ADR-0001](0001-development-is-adr-driven.md)):
an architectural decision is written down, with the alternatives it beat,
**before** code depends on it.

## What belongs here

An ADR records a constraint that

1. is enforced in more than one place, so no single file explains it,
2. has a plausible alternative someone will propose again, and
3. is expensive to reverse (code, published contract, user workflow).

Reasoning that fails this test goes into a comment at the decision site or the
changelog.

## Status values

| Status | Meaning |
|---|---|
| `Proposed` | Written, not yet agreed. Code may be spiked against it, not merged. |
| `Accepted` | Agreed. Code builds on it. |
| `Open` | A recorded question with the options laid out. **Nothing builds on either side** until it moves to `Proposed`/`Accepted`. |
| `Superseded by ADR-NNNN` | Reversed. The file stays; its outcome is never edited. |

An ADR describes a decision at the moment it was taken. To change the outcome,
write a new ADR and supersede the old one.

## Numbering and format

Four digits, monotonic, never reused. Copy [`0000-template.md`](0000-template.md),
keep it to about a page. `Enforced by` lists every place the constraint lives;
before code exists it names where it *will* live.

## Index

| ADR | Title | Status |
|---|---|---|
| [0001](0001-development-is-adr-driven.md) | Development is ADR-driven | Accepted |
| [0002](0002-processes-run-from-immutable-artifacts.md) | Processes run from immutable artifacts, never from the build output of the source tree | Proposed |
| [0003](0003-changes-flow-through-candidates.md) | Changes flow through candidates; the main slot only holds a stack that built and passed health | Proposed |
| [0004](0004-infrastructure-is-delegated-to-compose.md) | Infrastructure is delegated to Docker Compose | Proposed |
| [0005](0005-rust-for-daemon-and-cli.md) | Rust for daemon and CLI | Accepted |
| [0006](0006-dashboard-vue-same-origin.md) | The dashboard is a Vue 3 SPA served same-origin by the daemon | Accepted |
| [0007](0007-one-http-api-is-the-control-surface.md) | One HTTP API is the control surface; CLI, MCP and dashboard are its clients | Proposed |
| [0008](0008-background-workers-run-only-in-the-active-slot.md) | Background workers run only in the active slot | Proposed |
| [0009](0009-the-daemon-owns-every-process-tree.md) | The daemon owns every process tree it starts | Proposed |
| [0010](0010-promotion-mechanism.md) | Promotion mechanism: restart on the fixed port or switch a reverse proxy | Open |
| [0011](0011-platform-windows-native-first.md) | Windows-native first; WSL is decided by measurement | Open |
| [0012](0012-shared-infrastructure-between-lanes.md) | Which infrastructure may be shared between lanes | Open (deferred) |
| [0013](0013-schema-changes-hold-promotion.md) | A build that changes schema inputs is not promoted automatically | Proposed |
