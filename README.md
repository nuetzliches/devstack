# devstack

A local development orchestrator for multi-service stacks: infrastructure in
containers, application processes built from immutable artifacts, and a
**main slot that only ever holds a stack that built and passed its health
checks**. Changes flow through candidate builds that are cancelled the moment a
newer change lands.

> **Status: 0.2.0 (phase 2).** Daemon, CLI and MCP server run a stack on Windows
> and Linux. Saved edits are built in the background and swapped in behind a
> proxy only once they are healthy, without a failed request; a broken build
> never replaces what runs. A dashboard (a progressive web app served by the
> hub) shows every stack live. Lanes for several worktrees are phase 3. See
> [`docs/plan.md`](docs/plan.md) and the decision log in
> [`docs/adr/`](docs/adr/README.md).

## Install

Windows (PowerShell) and Linux (x86_64, static build), or see
[the landing page](https://nuetzliches.github.io/devstack/), which also tells whether
devstack runs on your machine. The scripts check the download against the
release's `SHA256SUMS`; run them again to update.

```powershell
# Windows: installs to %LOCALAPPDATA%\Programs\devstack and adds it to PATH
irm https://github.com/nuetzliches/devstack/releases/latest/download/install.ps1 | iex
```

```sh
# Linux: installs to ~/.local/bin
curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
```

A specific version: download the script and run it with `-Version 0.3.0`
(`--version 0.3.0` on Linux). From source:
`cargo install --git https://github.com/nuetzliches/devstack devstack`.

The installer also registers the dashboard hub to start at logon and a
`devstack://` link the dashboard uses to start it (`-NoHubIntegration` /
`DEVSTACK_NO_HUB_INTEGRATION=1` skip that). After installing, `devstack doctor`
in a repository says whether the machine is ready for its stack.
`devstack self uninstall` removes devstack again (hub, its registrations, the
per-user state, the PATH entry and the binary; repositories keep `.devstack/`).

An update under a running daemon is fine; `devstack down` and `devstack up`
switch to the new version. A stack file can name the oldest devstack it needs
(`[stack] requires = "0.1.0"`); an older one then asks to be updated.

## Use

A stack file `devstack.toml` in the repository root names the Compose files of
the infrastructure and the services devstack builds and runs
([example](examples/fixture-stack/devstack.toml); `devstack schema` prints the
JSON Schema).

```
devstack init             # a first devstack.toml from the repository (TODOs to work through)
devstack doctor           # is this machine ready: Docker, Compose, tools, ports, hub, PATH
devstack check            # validate the stack file (with --compose: the Compose groups too)
devstack up               # daemon, infrastructure, autostart services
devstack status
devstack ui               # the dashboard in the browser (every stack, live)
devstack hub install      # hub at logon + `devstack://` start link (up does it once)
devstack skills install   # agent skills for this repository, from the stack file
devstack restart api      # rebuild now; the running instance is replaced once the new one is healthy
devstack promote api      # take over a candidate held back by a schema change (ADR-0013)
devstack rollback api     # back to the build that ran before the last replacement
devstack logs api --follow
devstack logs --grep timeout  # every service's log, matching lines
devstack update           # the latest release; the dashboard says when there is one
devstack logs --daemon    # devstack's own decisions with their reasons (--hub: the dashboard hub)
devstack run reset        # a command from the stack file
devstack top --watch      # CPU, memory and disk per service, container, group and lane
devstack env api --export dotenv   # run the service without devstack (ADR-0015)
devstack down --infra
```

### How a change arrives

A service with `watch` patterns is rebuilt when a watched file changes (after a
quiet period of 2 s; a newer change kills the build in progress). The new build
starts as a **candidate** on an internal port without background workers; once
it is healthy, the proxy on the service's fixed port switches to it, the old
instance drains and stops, and a second start with workers on takes over the
same way (ADR-0003, ADR-0008, ADR-0010). Until then `devstack status`, MCP and
the dashboard say `not yet active: building | starting | failed | held`, and
every response carries `x-devstack-build` and, while something is pending,
`x-devstack-pending`. A change to `promote.hold_if_changed` paths (a schema
migration) waits for `devstack promote`.

### Agents (MCP)

`devstack mcp` is an MCP server on stdio. It is a client of the daemon's API
like the CLI: every tool maps to one endpoint (`status`, `up`, `down`,
`start_service`, `stop_service`, `restart_service`, `promote_service`,
`rollback_service`, `logs`, `diagnostics`, `doctor`, `run_command`, `usage`, `hub_status`, `hub_start`). Register it in the repository, e.g. `.mcp.json`:

```json
{ "mcpServers": { "devstack": { "command": "devstack", "args": ["mcp"] } } }
```

A command with a confirmation text in the stack file (a database reset, say)
only runs when the agent passes `confirm: true`, which it is told to do only
after the user agreed.

### Agent skills

`devstack skills install` writes two skills into the repository
(`.claude/skills/`, `--dir` for another place), filled in from the stack file:

- **`devstack`**: install check, operating the stack, "is my change live",
  this stack's services, commands and start checks, troubleshooting, and the
  rule to ask the user before reporting a devstack problem upstream.
- **`devstack-config`**: editing the stack file, with the rules that are easy to
  get wrong and a link to the reference, [docs/stack-file.md](docs/stack-file.md).

Each file carries a marker line; installing again refreshes it, and
`devstack status` (and the MCP status) notes when an installed skill no longer
matches this devstack or the stack file. Project specifics go into a skill of
the project's own, which devstack never touches. Commit the generated skills
with the repository.

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

## License

MIT or Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE)). Security reports: [SECURITY.md](SECURITY.md).
