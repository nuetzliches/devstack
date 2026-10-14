# devstack

A local development orchestrator for multi-service stacks: infrastructure in
containers, application processes built from immutable artifacts, and a
**main slot that only ever holds a stack that built and passed its health
checks**. Changes flow through candidate builds that are cancelled the moment a
newer change lands.

> **Status: planning.** There is no code yet. Start with
> [`docs/plan.md`](docs/plan.md) and the decision log in
> [`docs/adr/`](docs/adr/README.md).

## Why

Most local orchestrators run a service straight out of the build output of the
working tree and restart it whenever a file changes. With one developer that is
tolerable. With an IDE, several terminals and several AI agents editing the same
tree at once, it produces a stack that is half-built, file-locked, restarted
three times in one second, and impossible to ask "is my change actually
running?". [`docs/problem-statement.md`](docs/problem-statement.md) describes
the failure modes this project exists to remove.

## Shape (planned)

```
devstack (CLI) ─┐
dashboard (Vue) ├──► devstackd (Rust, one HTTP API on loopback)
MCP (agents) ───┘         │
                          ├── infra:  docker compose (long-lived, health-gated)
                          ├── build:  snapshot → build → artifact <build-id>
                          └── run:    main slot (fixed ports) + candidates
```

## Repository conventions

- Documentation, code and commits in English.
- ADR-driven: an architectural decision is recorded before it is built. See
  [`docs/adr/README.md`](docs/adr/README.md) and [`AGENTS.md`](AGENTS.md).
- The project is generic. Downstream stacks that use it keep their stack file,
  compose file and secrets in their own repositories.
