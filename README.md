# devstack

**Your whole stack, always running the code you saved, never a broken build.**
Native services, their containers, every git worktree beside it, and when your
laptop runs out of room, the team's server: one CLI, one dashboard, one MCP
server for your coding agents.

```
devstack up        # infrastructure, builds, every service: healthy or it says why
devstack ui        # every stack, live, in the browser
```

- **Save, and it is live, or it says why not.** A change builds as a
  *candidate* beside the running instance and takes over only once it is
  healthy. A compiler error leaves the old build answering; `devstack status`
  says `not yet active: building | failed | held` until your change runs.
- **Every worktree is a lane.** `git worktree add` and `devstack up` in it: the
  same stack on its own ports, with its own containers and data, beside the
  main one. Ten agents on ten branches, no collisions.
- **Lanes on other machines.** `devstack up --on build-server` runs a
  worktree's lane on a machine of the team's pool, its ports forwarded to
  yours as if it ran locally. Move it back with one command, share it with a
  reviewer, give it the main ports.
- **Made for agents.** An MCP server and generated skills let coding agents
  start, check, fix and move the stack, and know whether their change is live.
- **Your files, no lock-in.** Infrastructure comes from the Compose files you
  deploy with; services from a short `devstack.toml`. `devstack env api
  --export dotenv` runs any service without devstack.
- **Windows and Linux, natively.** One static binary, no WSL required. macOS
  is in preview.

> **Before 1.0** · local stacks, lanes and the pool are in daily use on .NET, Node and
> Angular stacks. See [what is planned](docs/plan.md) and
> [the decisions behind it](docs/adr/README.md).

## Install

```powershell
# Windows: to %LOCALAPPDATA%\Programs\devstack, on PATH
irm https://github.com/nuetzliches/devstack/releases/latest/download/install.ps1 | iex
```

```sh
# Linux (x86_64, arm64; static) and macOS: to ~/.local/bin
curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
```

```sh
# macOS and Linux, with Homebrew
brew install nuetzliches/tap/devstack
```

On Windows, `winget install nuetzliches.devstack` installs the same build
(releases after 0.15.0, once winget's catalogue has them, usually a few days
after the release); `devstack update` then updates it through winget, and
the first `devstack up` registers the hub as the installer would. A copy
Homebrew installed is updated the same way, through `brew upgrade`.

**macOS is a preview** ([ADR-0054](docs/adr/0054-macos-is-a-developer-platform-in-preview.md), [what a tester checks](docs/macos.md)):
Apple Silicon and Intel, macOS 11 or later, Docker Desktop, OrbStack or
Colima. None of the maintainers has a Mac: CI checks what it can, and
[reports](https://github.com/nuetzliches/devstack/issues/66) check the rest.
Desktop notifications, the `devstack://` link and a Mac as a pool node are
not there yet. The binary is not signed: install it with Homebrew or
`install.sh` (neither marks it for Gatekeeper); a copy downloaded with a
browser is blocked until `xattr -d com.apple.quarantine devstack`.

The scripts and `devstack update` check the download against the release's
`SHA256SUMS`: that proves the archive is the one the release lists (not
damaged or swapped on the way), not who built it, since both come from the
same release. Each release also carries a build provenance attestation; with
the [GitHub CLI](https://cli.github.com) installed and logged in, the scripts
verify it (`gh attestation verify <archive> --repo nuetzliches/devstack`):
built by this repository's release workflow (releases from 0.10.0 on). The
Windows executable is not code-signed, so SmartScreen or an endpoint
protection product may warn about it at first ([SECURITY.md](SECURITY.md#release-integrity)). The
[landing page](https://nuetzliches.github.io/devstack/) tells whether your
machine is ready. `devstack update` (or the dashboard) installs a newer
release; `devstack self uninstall` removes devstack again. From source: `cargo
install --git https://github.com/nuetzliches/devstack devstack` (without the
dashboard: it needs `npm ci && npm run build` in `ui/` before the build;
`devstack update` leaves such a binary alone).

The installer registers the dashboard **hub** to start at logon (on macOS a
LaunchAgent, for a Homebrew install too: not `brew services`) and a
`devstack://` link the dashboard uses to start it. To skip both, set
`DEVSTACK_NO_HUB_INTEGRATION=1` before the install command (Windows also
takes `-NoHubIntegration` when the script is run as a file); the choice is
kept, so `devstack up` does not register them later either. `devstack hub
install` adds them, `devstack hub uninstall` removes them again.

## Start

```sh
devstack init              # a first devstack.toml from the repository (TODOs to work through)
devstack doctor            # is this machine ready: Docker, Compose, tools, ports
devstack up                # daemon, infrastructure, every autostart service
devstack ui                # the dashboard
devstack skills install    # skills for your coding agent, filled in from the stack file
```

A stack file names the Compose files of the infrastructure and the services
devstack builds and runs:

```toml
version = 1

[stack]
name = "shop"

[compose.app]
project  = "shop-dev"
files    = ["deploy/compose.yml"]
services = ["db"]

[services.api]
build  = "dotnet build src/Api -c Debug"
output = "src/Api/bin/Debug/net9.0"
run    = "{artifact}/Api"
port   = 5000
health = { http = "/health", timeout = "60s" }
watch  = ["src/Api/**"]
env    = { ConnectionStrings__Db = "Host={compose.app.db.5432}" }

[services.web]
kind = "devserver"
run  = "npm run dev -- --port {port}"
port = 4200
```

The full reference: [docs/stack-file.md](docs/stack-file.md); `devstack
schema` prints the JSON Schema for your editor.

## Every day

```sh
devstack status                    # every service; "not yet active" until your change runs
devstack logs api --follow         # --grep timeout across all, --since 2h for earlier instances
devstack restart api               # rebuild now; the running one stays until the new one is healthy
devstack promote api               # take over a candidate held back by a schema change
devstack rollback api              # back to the build before the last replacement
devstack builds api                # did my change ever go live, since when is it broken
devstack hot-reload api on         # the toolchain's own watcher, for a debugging session
devstack run reset                 # a command of the stack file (a database reset, say)
devstack top --watch               # CPU, memory, disk per service, container, group and lane
devstack env api                   # what a service gets, with origins
devstack logs --daemon             # devstack's own decisions with their reasons
devstack down [--infra] [--all]    # running lanes keep the daemon; --all stops them too
```

## Lanes: every worktree, side by side

```sh
git worktree add ../shop-login -b login-fix     # a worktree is a lane at once
cd ../shop-login && devstack up                 # its own builds, ports +100, its own containers
devstack lane start login-fix                   # the same, from the main tree
devstack lane list                              # every lane, where it runs, who has the main ports
devstack promote --lane login-fix               # it answers on the main ports; --lane main gives them back
devstack lane stop login-fix
```

A lane runs from a copy of its worktree, so git can remove the worktree while
it runs. It gets its own Compose project and data (seed jobs fill it), and no
background workers unless it holds the main ports.

## The pool: lanes on other machines

```sh
devstack join https://10.0.0.5:4260 --fingerprint <fp>   # once; an admin approves
devstack up --on build-server        # in a worktree: everything of the lane runs there
devstack up --on pool                # the coordinator picks a node
devstack lane move login-fix --to local
devstack lane share login-fix --with team:qa
devstack open login-fix web          # a lane someone shares with you (connects it)
devstack pool disconnect login-fix   # its ports here are free again
```

One machine runs the coordinator, servers run a node agent; the dashboard's
pool area admits members, shows capacity (GPUs included) over time, and sets
what each node runs (who, which repositories, limits, network). Several pools
per machine, a customer's beside the team's. Setup and everything else:
**[docs/pool.md](docs/pool.md)**.

## Agents

`devstack mcp` is an MCP server on stdio, a client of the same API as the CLI:
`status`, `up` (with `on` for a node), `down`, `start_service`,
`stop_service`, `restart_service`, `promote_service`, `rollback_service`,
`hot_reload`, `logs`, `builds`, `diagnostics`, `doctor`, `run_command`,
`usage`, `lanes`, `lane_start`, `lane_stop`, `lane_move`, `lane_share`,
`promote_lane`, `placement`, `hub_status`, `hub_start`.

```json
{ "mcpServers": { "devstack": { "command": "devstack", "args": ["mcp"] } } }
```

`devstack skills install` writes two skills into the repository
(`.claude/skills/` by default; `[agents] skills` in the stack file names the
folders, `.agents/skills` for Codex and most other agents):
**`devstack`** (operating this stack, "is my change live", troubleshooting)
and **`devstack-config`** (editing the stack file). They are filled in from
the stack file; `devstack status` says when they are out of date. A command with a confirmation text in the stack file runs only
when the agent passes `confirm: true`, which it may only do after you agreed.

## Commands

Every command and flag: `devstack --help`, `devstack <command> --help`.

| Command | What it does |
|---|---|
| `devstack init`, `check [--compose]`, `schema` | write a first stack file; validate it (with `--compose` every Compose group on its own too); print its JSON Schema |
| `devstack doctor` | what this machine lacks for this stack, with the fix; exits 1 when something fails |
| `devstack up [--on <node>\|pool] [--workers] [--force] [--profile <name> [--run]] [--yes]`, `down [--infra] [--all]`, `status` | start (in a worktree: its lane, here or on a node; with a profile of the stack file, for a purpose such as an acceptance run), stop (`--all`: the lanes too), show the stack |
| `devstack start\|stop\|restart <service\|group>`, `promote`, `rollback`, `hot-reload` | one service or group; `restart --rebuild` builds again although an artifact for the same inputs exists |
| `devstack build <service>` | sync the main tree's snapshot and build an artifact, without starting it (in a lane: `devstack restart <service>` in its worktree) |
| `devstack builds <service>`, `logs`, `top`, `env <service> [--export dotenv]` | what ran, what it printed, what it uses, what it gets |
| `devstack run <command>` | a command of the stack file |
| `devstack lane list\|start\|stop\|reset\|prune\|move\|share` | the lanes (worktrees), see above; `reset` throws a stopped lane's data away, `prune` removes what gone lanes left behind |
| `devstack ui [--lane <name>] [--browser <name\|path>] [--link\|--print]` | open and pair the dashboard, on this stack (in a worktree on its lane, or on `--lane`); the browser is `--browser`, else `DEVSTACK_BROWSER`, else `BROWSER`, else the dashboard's installed app when a Chromium-family browser has it (Linux, Windows; [ADR-0061](docs/adr/0061-the-dashboard-opens-in-its-installed-app.md)), else the system's default. `--link` prints the lasting links to the stack or lane instead (the page, and the `devstack://open` link that pairs first); `--print` the one-time pairing link. `status`, `up` and `lane start` end with the page as `dashboard: <url>` |
| `devstack hub [status\|stop\|start\|install\|uninstall]` | the hub that serves the dashboard: without an action in the foreground; `stop` leaves the stacks running; `install` registers it at logon and the `devstack://` link (`--no-autostart`: the link only), `uninstall` removes both, and `devstack up` then leaves them alone |
| `devstack hub tray [on\|off]` | the tray icon ([ADR-0060](docs/adr/0060-the-tray-is-a-client-of-the-hub.md)): the state of every stack in the icon's colour, a menu to open the dashboard (in its installed app when there is one, as `devstack ui`), start or stop a stack and open its folder. The hub starts it (`devstack tray`); `off` ends it and keeps it off, `on` brings it back. Windows, Linux (a StatusNotifierItem: KDE, Xfce, GNOME with the AppIndicator extension) and macOS |
| `devstack skills install [--dir] [--force]`, `skills check` | write the agent skills; check that they match this devstack and stack file (exits 1 when not) |
| `devstack mcp` | the MCP server (above) |
| `devstack join <url> --fingerprint <fp> [--name] [--pool] [--telemetry] [--no-wait]` | ask a pool to admit this machine ([pool.md](docs/pool.md#join-a-pool)) |
| `devstack pool [shared\|telemetry on\|off]`, `open <lane> [<service>]` | the pool as this machine sees it; lanes others share with you |
| `devstack node init\|serve\|fingerprint\|agent` | run a pool's coordinator or a node agent ([pool.md](docs/pool.md)) |
| `devstack node members [approve\|remove\|role\|tags\|node\|policy]` | administer a pool on its coordinator; `node <id> on\|off` grants a member to run the pool's lanes |
| `devstack update [--check] [--version <v>]`, `self uninstall [--with-coordinator]` | install a newer release (or only say whether one exists) and move the hub onto it; remove devstack for this user (its pool memberships go too; a coordinator kept in the per-user state only with `--with-coordinator`) |

## Why

Most local orchestrators run a service straight out of the working tree's
build output and restart it whenever a file changes. With one developer that
is tolerable. With an IDE, several terminals and several AI agents editing the
same tree, it gives a stack that is half-built, file-locked, restarted three
times a second, and impossible to ask "is my change running?".
[docs/problem-statement.md](docs/problem-statement.md) describes the failure
modes devstack removes.

## Shape

```
devstack CLI ──┐                    ┌── infrastructure: docker compose, health-gated
MCP (agents) ──┼──► devstackd ──────┼── builds: snapshot → build → artifact <build-id>
dashboard ─────┘    one per repo,   └── services: main slot (fixed ports) + candidates,
   │                loopback API          lanes (worktrees) on their own ports
   └── hub (every stack, the pool) ──► coordinator ──► node agents (lanes of others)
```

## Environment

| Variable | Effect |
|---|---|
| `DEVSTACK_BROWSER` | the browser `devstack ui` pairs (`chrome`, `edge`, `firefox`, `brave`, a path), and that a click on a notification or the tray opens; set, it wins over the dashboard's installed app |
| `DEVSTACK_HUB_PORT` | the hub's port (default 4250) |
| `DEVSTACK_NO_HUB` | `devstack up` does not start the dashboard hub |
| `DEVSTACK_NO_HUB_INTEGRATION` | the installer registers no hub at logon and no `devstack://` link (and records that); `devstack up` registers neither while it is set |
| `DEVSTACK_NO_TRAY` | the hub does not start the tray icon (`devstack hub tray off` keeps it off for good) |
| `DEVSTACK_NO_NOTIFY` | the hub shows no desktop notifications (they are set per kind with `devstack hub notify` or the bell in the dashboard) |
| `DEVSTACK_NO_UPDATE_CHECK` | no check for a newer release |
| `DEVSTACK_OTLP_ENDPOINT` | devstack's own diagnostic log also to this OTLP/HTTP collector |
| `DEVSTACK_HUB_PING_ORIGINS` | more origins (comma-separated) that may ask whether a hub runs, for a copy of the landing page |
| `DEVSTACK_POOL_DIR`, `DEVSTACK_COORDINATOR_DIR`, `DEVSTACK_TELEMETRY_*` | the pool: see [docs/pool.md](docs/pool.md#reference) |

## Documentation

| | |
|---|---|
| [docs/stack-file.md](docs/stack-file.md) | the stack file, every key |
| [docs/pool.md](docs/pool.md) | the pool: coordinator, nodes, lanes on other machines, sharing, policies |
| [docs/dashboard-api.md](docs/dashboard-api.md) | the hub's and the daemon's HTTP API |
| [docs/plan.md](docs/plan.md) | architecture, phases, what is open |
| [docs/adr/](docs/adr/README.md) | the decision log |
| [CHANGELOG.md](CHANGELOG.md) | what changed in each release |
| [SECURITY.md](SECURITY.md) | what devstack exposes, what leaves the machine, the pool's trust model, reporting a vulnerability |

## Contributing

Documentation, code and commits in English. Architectural decisions are
recorded as ADRs before they are built ([AGENTS.md](AGENTS.md)). The project
is generic: downstream stacks keep their stack file, Compose files and secrets
in their own repositories.

## License

MIT or Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE)). Security reports: [SECURITY.md](SECURITY.md).
