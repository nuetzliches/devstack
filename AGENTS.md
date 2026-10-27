# Guidance for AI Assistants

## Core expectations

1. **Target stack:** Rust (latest stable, edition 2024) for daemon and CLI
   ([ADR-0005](docs/adr/0005-rust-for-daemon-and-cli.md)). The dashboard is
   Vue 3 + TypeScript under `ui/`, served by the hub for every stack
   ([ADR-0006](docs/adr/0006-dashboard-vue-same-origin.md),
   [ADR-0021](docs/adr/0021-a-hub-serves-the-dashboard.md)).
2. **Language:** documentation, commits and code comments in English.
3. **Review card before the code, ADR after it**
   ([ADR-0070](docs/adr/0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)):
   read [`docs/adr/README.md`](docs/adr/README.md) before proposing a change
   to anything listed in its index. A change with a point in a card category
   (trust, concept, contract, term) starts with a review card of three to
   seven one-sentence points, shown to the owner and put in the issue or pull
   request; do not bring the owner an ADR text to review. List what you
   decided on your own. Merge only after the owner approved the points; then
   the ADR records what was built. An ADR with status `Open` is a question,
   not a licence: do not build on either side of it.
4. **Dependencies:** MIT-compatible licences only, latest stable versions.
5. **Code style:** `cargo clippy` clean, `cargo fmt` formatted.
6. **Windows is a first-class target.** Every feature works on native Windows
   (process control, paths, file watching, signals) before it is called done.
   See [ADR-0011](docs/adr/0011-platform-windows-native-first.md).
   macOS is a preview ([ADR-0054](docs/adr/0054-macos-is-a-developer-platform-in-preview.md)):
   no maintainer has a Mac, so platform code gets a macOS module (or says
   "not on macOS yet") and must keep the `macos-14` CI jobs green.

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
| `docs/stack-file.md` | The stack file reference (keep in step with `crates/devstack-config`) |
| `docs/pool.md` | The pool: coordinator, nodes, lanes on other machines |
| `docs/dashboard-api.md` | The hub's and the daemon's HTTP API |
| `docs/releasing.md` | The release checklist: the release pull request, the tag, what follows |
| `skills/` | The agent skills `devstack skills install` writes (keep in step with the CLI) |
| `docs/problem-statement.md` | The failure modes this project removes, and the requirements derived from them |
| `docs/adr/` | Decision log |
