# devstack

A local development orchestrator for multi-service stacks: infrastructure in
containers, application processes built from immutable artifacts, and a
**main slot that only ever holds a stack that built and passed its health
checks**. Changes flow through candidate builds that are cancelled the moment a
newer change lands.

> **Status: phase 1 in progress.** Daemon, CLI and MCP server run a stack's
> main slot (builds on demand, health, logs, commands, resource usage) on
> Windows and Linux; candidates, promotion and the dashboard are phase 2. See
> [`docs/plan.md`](docs/plan.md) and the decision log in
> [`docs/adr/`](docs/adr/README.md).

## Use

A stack file `devstack.toml` in the repository root names the Compose files of
the infrastructure and the services devstack builds and runs
([example](examples/fixture-stack/devstack.toml); `devstack schema` prints the
JSON Schema).

```
devstack check            # validate the stack file (with --compose: the Compose groups too)
devstack up               # daemon, infrastructure, autostart services
devstack status
devstack restart api      # rebuild; the running instance is replaced only if the build succeeds
devstack logs api --follow
devstack run reset        # a command from the stack file
devstack top --watch      # CPU, memory and disk per service, container, group and lane
devstack env api --export dotenv   # run the service without devstack (ADR-0015)
devstack down --infra
```

### Agents (MCP)

`devstack mcp` is an MCP server on stdio. It is a client of the daemon's API
like the CLI: every tool maps to one endpoint (`status`, `up`, `down`,
`start_service`, `stop_service`, `restart_service`, `logs`, `run_command`,
`usage`). Register it in the repository, e.g. `.mcp.json`:

```json
{ "mcpServers": { "devstack": { "command": "devstack", "args": ["mcp"] } } }
```

A command with a confirmation text in the stack file (a database reset, say)
only runs when the agent passes `confirm: true`, which it is told to do only
after the user agreed.

## Why

Most local orchestrators run a service straight out of the build output of the
working tree and restart it whenever a file changes. With one developer that is
tolerable. With an IDE, several terminals and several AI agents editing the same
tree at once, it produces a stack that is half-built, file-locked, restarted
three times in one second, and impossible to ask "is my change actually
running?". [`docs/problem-statement.md`](docs/problem-statement.md) describes
the failure modes this project exists to remove.

## Shape

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
