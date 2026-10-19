---
name: devstack
description: "Runs and watches this repository's local stack with devstack ({{devstack:stack_file}}): checks that devstack is installed and current, starts and stops the stack, tells whether a code change is live yet, reads logs, promotes or rolls back builds, runs the stack's commands, and reports devstack problems upstream with the user's consent. Use it whenever the local stack, a service, its logs or its dashboard is involved, e.g. \"start the stack\", \"is my change live\", \"the API does not pick up my change\", \"restart the service\", \"show the logs\", \"run the seed\", \"devstack up/status/down\", \"the dev stack does not start\", \"port is taken\", \"Docker is not running\", and whenever a devstack command or its MCP server fails. Not for editing {{devstack:stack_file}} itself (skill `devstack-config`)."
---

<!-- {{devstack:marker}} -->

# devstack: run the local stack ({{devstack:stack_name}})

devstack runs the stack described in `{{devstack:stack_file}}`: containers from
Compose, native processes built into immutable artifacts, and commands. A saved
code change is built in the background and replaces the running instance only
once it is healthy, so the stack keeps answering while you work.

## 1. Is devstack installed and current? (check first)

```bash
devstack --version          # this file needs {{devstack:requires}} or newer
devstack doctor             # Docker, Compose, the tools this stack runs, ports, hub, PATH
```

Missing (`command not found`) or too old: tell the user how to install, do not
install it yourself.

```powershell
# Windows
irm https://github.com/nuetzliches/devstack/releases/latest/download/install.ps1 | iex
```

```bash
# Linux
curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
```

https://nuetzliches.github.io/devstack/ shows the same commands and whether devstack runs on
the user's machine.

The installer updates in place; a running daemon keeps the old version until
`devstack down` + `devstack up`. A freshly installed devstack may not be on the
PATH of an already running agent process: then use the full path
(`%LOCALAPPDATA%\Programs\devstack\devstack.exe`, `~/.local/bin/devstack`).

## 2. Operate

Run from the repository root (devstack looks for `{{devstack:stack_file}}` there; `-f` otherwise).

| Command | Effect |
| --- | --- |
| `devstack up` | Docker check, infrastructure, jobs, every autostart service; starts the dashboard hub. First run takes minutes (images, builds), later ones well under a minute |
| `devstack status` | state per service and container; `-> not yet active: …` means the latest change is not running yet |
| `devstack logs <service> --follow` | console output, then every new line (`-n 500` for more history, `--grep X` for matching lines only) |
| `devstack logs --grep <text>` | matching lines of every service's log ("where did this error come from"); with a service only its log |
| `devstack logs <service> --since 2h` | every instance of that time, not only the current one: what a crashed or replaced instance printed (`--build <id>`: that build's log and its instances; `--list`, `--log <file>`) |
| `devstack logs <container>` | a Compose container's output by its service name (`docker compose logs`, timestamped; `--grep`, `--since 30m`) |
| `devstack builds <service>` | the service's builds, newest first: live, failed (with the reason), superseded by a newer change, held; answers "did my change ever go live" and "since when is it broken" |
| `devstack hot-reload <service> on\|off` | for a debugging session: the toolchain's watcher (e.g. `dotnet watch`) from the source tree instead of builds and candidates; needs `hot_reload` in the stack file. Switch it off again afterwards |
| `devstack up` in a `git worktree` | starts that worktree's **lane** (every worktree is one) in the main tree's daemon: its own builds, ports moved by +100 (+200, ...), its own containers (a Compose project per lane, ports moved alike; `shared` services come from the main tree), no background workers (`--workers` turns them on; `--force` skips the memory check). Every command in the worktree then acts on the lane (`devstack lane list` shows all) |
| `git worktree add <path> -b <branch>` | a new worktree is a lane at once, not started (devstack never creates or removes worktrees; use git or your tool). A lane runs from a copy of its worktree, so `git worktree remove` works while it runs and stops it. `devstack lane stop <name>` stops a lane and keeps the worktree |
| `devstack lane start <name> [--on <node>]` | start a lane from the main tree (or anywhere in the repository), as `devstack up` in its worktree does; `status` in the main tree lists the lanes in one line. **To run another branch or commit beside the main stack** (an end-to-end run against a given commit, a review): `git worktree add <dir> <commit>`, `devstack lane start <name>` (its ports are the main ports plus its offset, `devstack lane list`), test against those, `devstack lane stop <name>`, `git worktree remove <dir>`. **For an acceptance run** use the stack file's profile if it has one (`status` lists none; `[profiles]` in the file): `devstack lane start <name> --profile <p> --run` starts the lane as the profile says (a build instead of a dev server, workers, fresh data, the suite's env) and runs its suite command. A worktree another tool or script created is a lane too: starting it gives its own ports, containers and database without building a separate environment |
| `devstack lane reset <name>` / `devstack lane prune [--older-than 7d] [--dry-run]` | throw a stopped lane's data away (it starts fresh, seeded again); remove what lanes whose worktree is gone left behind, here and on the pool's nodes. A lane on a node keeps its data there between starts. Ask the user before either: data goes |
| `devstack promote --lane <name>` | the lane's services, dev servers included, answer on the main ports; ask the user first, `--lane main` gives them back. A lane on a node may take them only on a dedicated node (a server) |
| `devstack up --on <node>` in a worktree | the lane runs on that machine of the pool (`pool`: the coordinator picks one, `<pool>:<node>` in another pool): build, services, its own containers; its ports are forwarded here as if it ran locally. Ask the user first: the worktree's files go to that machine, untracked env files git does not ignore included |
| `devstack lane move <name> --to <node\|local>` | move a started lane: a cold start there that takes over once ready; its data comes from the seed jobs again |
| `devstack lane share <name> --with <id\|tag\|*> [--view <id\|tag\|*>]` | let others in the pool see and open the lane's services (they do not control it); `--view`: they may also view its state and logs, read only. Ask the user first. Without `--with` and `--view`: nobody again |
| `devstack pool` / `devstack pool shared` / `devstack open <lane> [service]` | the pools and their machines; lanes others share with you; open one of their services here |
| `devstack logs --daemon` | devstack's own log: every decision with its reason (`--grep <service>`, `--hub` for the dashboard hub) |
| `devstack restart <service>` | rebuild now; the running instance stays until the new one is healthy |
| `devstack restart <group>` | every service of a group from the stack file, built at the same time, and its containers (`start`/`stop` take a group too) |
| `devstack promote <service>` | take over a candidate that is held (schema change, see below) |
| `devstack rollback <service>` | back to the build before the last replacement, without building |
| `devstack start` / `stop <service>` | one service |
| `devstack run <command>` | a command of the stack file (list below) |
| `devstack top` | CPU, memory, disk per service, container, group and lane |
| `devstack ui` | dashboard in the browser (pairs the browser once); `--browser chrome\|edge\|firefox\|<path>` or `DEVSTACK_BROWSER` picks the browser, `--print` only prints the link |
| `devstack update` | install the newest devstack (`--check` only asks); stacks switch at their next restart |
| `devstack down` | stop the services and the daemon; containers keep running. `down --infra` removes them too. In the main tree, lanes that run keep the daemon and the infrastructure (only the main tree's services stop); `down --all` stops the lanes too: ask first, they may be someone else's |

### Is my change live?

Changes arrive by themselves: saving a file under a service's `watch` paths
starts a candidate build. **Before testing a change, check that `devstack status`
shows no `not yet active` line for the service.** The states:

| Candidate | Meaning | What to do |
| --- | --- | --- |
| `changed`, `building`, `starting`, `switching` | on its way | wait; `devstack status` again |
| `failed` | build or health check failed; the old build keeps running | read the reason in `devstack status` (compiler output), fix the code |
| `held` | built and healthy, waits for `promote` (it touched a path like a migration) | ask the user, then `devstack promote <service>` |

A `devserver` (hot reload) has no candidates: it picks up changes itself.

A folder that was moved or renamed (`mv`, `git mv`, an IDE's "move folder")
counts as one change, on the folder's own path. A `watch` pattern like
`src/**/*.cs` does not match it, so no candidate starts and `devstack status`
shows nothing pending although the running build lacks the folder's files.
After moving a folder with sources, run `devstack restart <service>`.

## 3. This stack

{{devstack:services_table}}

{{devstack:held_paths}}

{{devstack:commands_table}}

Commands with a confirmation text (and anything else that deletes data) run only
after the user said yes; never answer a confirmation on the user's behalf.

## 4. After starting: verify (required before saying "it runs")

```bash
devstack status             # every service healthy / running / completed, no "not yet active"
{{devstack:start_check}}
```

Check, do not assume: if something "does not run", `devstack status` and the
port tell within a second, and they are the evidence to quote.

## 5. Agents: MCP

`devstack mcp` is an MCP server (stdio) with `status`, `up` (`on` for a lane on a
node), `down`, `start_service`, `stop_service`, `restart_service`,
`promote_service`, `rollback_service`, `hot_reload`, `logs`, `builds`,
`diagnostics`, `doctor`, `run_command`, `usage`, `lanes`, `lane_start`,
`lane_stop`, `lane_move`, `lane_share`, `promote_lane`, `hub_status`,
`hub_start`. `lane_start` and `lane_stop` work from the main tree too: an
agent there runs another worktree's lane without moving to it.
Register it once (project `.mcp.json` or user configuration):

```json
{ "mcpServers": { "devstack": { "command": "devstack", "args": ["mcp"] } } }
```

`run_command` for a command with a confirmation text runs only with
`confirm: true`, and that comes from the user, never from the agent.

## 6. When something goes wrong

| Symptom | Cause and fix |
| --- | --- |
| anything does not start, before guessing | `devstack doctor` (MCP `doctor`): what this machine lacks for this stack, one line each, with the fix |
| something happened and nothing says why (a restart, a switch, a stop) | `devstack logs --daemon --grep <service>` (MCP `diagnostics`): service and candidate states with reasons, every request that changed something, daemon start and stop; it works with the daemon stopped |
| `devstack up`: Docker is not running | `up` (CLI and MCP) starts Docker Desktop and waits; with Docker Engine as a Linux service it names `sudo systemctl start docker` for the user |
| "the daemon of this stack (pid …) does not answer" | it hangs: `devstack logs --daemon -n 50` says what it did last; end that process (ask the user first), then `devstack up` |
| a port is taken | another process (often a second copy of the stack, an IDE run, a leftover process) holds it; find it (`Get-NetTCPConnection -LocalPort <p>` / `ss -ltnp`) and ask before ending it |
| `failed` candidate | the old build keeps running; `devstack status` shows the reason, the build log is in `.devstack/logs/<service>/build-<id>.log` |
| `held` candidate | waits for `devstack promote <service>` after the user agreed |
| `devstack status`: "the stack file changed since the daemon started" | the daemon reads the file once: `devstack down` + `devstack up` (ask first if others use the stack) |
| a service exits right after start | `devstack logs <service> -n 200`; every start has its own log in `.devstack/logs/<service>/run-*.log` |
| dashboard: "hub not reachable" | `devstack ui` (or the "Start hub" button once `devstack hub install` ran) |
| this skill says it is outdated | `devstack skills install` refreshes it from the installed devstack and this stack file |
| dashboard: "runs devstack X, Y is installed" | the stack still runs the old daemon: "Restart stack" there, or `devstack down` + `devstack up` |
| a job `failed` after a long wait | jobs end after 30 minutes unless the stack file sets `timeout`; read its log, then ask the user before raising it (skill `devstack-config`) |
| `devstack down` names services that did not stop | they still run: `devstack status`, then `devstack stop <service>`; ask before ending a process yourself |
| `devstack up --on`: the node refuses the lane | the message names what the lane's Compose files ask that a node does not allow (privileged, host mounts, devices, ...) or that the pool has not granted that node; the lane can still run here (`devstack up`) |
| `devstack up --on` (or the MCP `up` with `on`) was interrupted or timed out while the lane started on the node | the start goes on there: `devstack lane list` shows its step; the same `devstack up --on <node>` again follows it to the end. A first start can take minutes (images pulled and built, containers waited for until healthy) |

## 7. Problems in devstack itself

devstack is young. **If you find a bug or a rough edge in devstack, or something
that would make the work between this repository, devstack and the agent harness
smoother (a missing MCP tool, an unclear message, a status that was wrong or did
not say enough), ask the user for permission to open an issue in
`nuetzliches/devstack`.** Do not open it without a yes.

What the issue needs: devstack version (`devstack --version`), OS, what you did,
what happened and what you expected, the relevant lines of `devstack status`,
`devstack logs --daemon` and the service log, and a minimal stack file excerpt
that shows it.

**Keep the project out of it.** The issue goes to another repository (and may
become public): no names of this project, its customers, hosts, internal paths,
ticket numbers or credentials. Describe the shape of the setup generically ("a
.NET API behind the proxy with a migrator job") and replace names in excerpts.

```bash
gh issue create -R nuetzliches/devstack --title "<what is wrong>" --body-file <file>
```
