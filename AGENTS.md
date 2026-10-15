# Guidance for AI Assistants

## Core expectations

1. **Target stack:** Rust (latest stable, edition 2024) for daemon and CLI
   ([ADR-0005](docs/adr/0005-rust-for-daemon-and-cli.md)). The dashboard is
   Vue 3 + TypeScript under `ui/`, served same-origin by the daemon
   ([ADR-0006](docs/adr/0006-dashboard-vue-same-origin.md)).
2. **Language:** documentation, commits and code comments in English.
3. **ADR-driven:** read [`docs/adr/README.md`](docs/adr/README.md) before
   proposing a change to anything listed in its index. A new architectural
   decision gets an ADR with status `Proposed` *before* the code that depends
   on it. An ADR with status `Open` is a question, not a licence: do not build
   on either side of it.
4. **Dependencies:** MIT-compatible licences only, latest stable versions.
5. **Code style:** `cargo clippy` clean, `cargo fmt` formatted.
6. **Windows is a first-class target.** Every feature works on native Windows
   (process control, paths, file watching, signals) before it is called done.
   See [ADR-0011](docs/adr/0011-platform-windows-native-first.md).

## Stack file changes

A change to the stack file format (`crates/devstack-config`) that makes an
existing file fail to load, or behave differently, is a **breaking change**.
Before making one, **ask the project owner whether devstack has users outside
its maintainers by now**: until it does, no migration is needed (raise
`[stack] requires` and say so); once it does, the rules of
[ADR-0023](docs/adr/0023-stack-file-compatibility.md) apply (deprecated aliases,
`version = 2`, `devstack migrate`). The frozen files in
`crates/devstack-config/tests/compat/` catch a break you did not intend.

## Public-repository hygiene

This repository may become public. It must not contain anything that identifies
or describes a downstream user's product, customers, hosts, service names,
internal paths, ticket numbers or credentials. Describe downstream experience
generically ("a downstream .NET + Angular stack"). Example configuration uses
invented service names (`api`, `web`, `worker`, `db`).

## Where things are

| Path | What |
|---|---|
| `docs/plan.md` | Architecture, phases, open questions |
| `docs/problem-statement.md` | The failure modes this project removes, and the requirements derived from them |
| `docs/adr/` | Decision log |
