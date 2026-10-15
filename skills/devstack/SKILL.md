---
name: devstack
description: Runs and watches this repository's local stack with devstack ({{devstack:stack_file}}): checks that devstack is installed and current, starts and stops the stack, tells whether a code change is live yet, reads logs, promotes or rolls back builds, runs the stack's commands, and reports devstack problems upstream with the user's consent. Use it whenever the local stack, a service, its logs or its dashboard is involved, e.g. "start the stack", "is my change live", "the API does not pick up my change", "restart the service", "show the logs", "run the seed", "devstack up/status/down", "the dev stack does not start", "port is taken", "Docker is not running", and whenever a devstack command or its MCP server fails. Not for editing {{devstack:stack_file}} itself (skill `devstack-config`).
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
devstack check              # validates the stack file, names a version that is too old
```

Missing (`command not found`) or too old: tell the user how to install, do not
install it yourself.

```powershell
# Windows (needs `gh` with access to the devstack repository while it is private)
gh release download -R nuetzliches/devstack -p install.ps1 -D $env:TEMP --clobber
& "$env:TEMP\install.ps1"
```

```bash
# Linux
gh release download -R nuetzliches/devstack -p install.sh -D /tmp --clobber && sh /tmp/install.sh
```

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
| `devstack logs <service> --follow` | console output (`-n 500` for more history) |
| `devstack logs --daemon` | devstack's own log: every decision with its reason (`--grep <service>`, `--hub` for the dashboard hub) |
| `devstack restart <service>` | rebuild now; the running instance stays until the new one is healthy |
| `devstack promote <service>` | take over a candidate that is held (schema change, see below) |
| `devstack rollback <service>` | back to the build before the last replacement, without building |
| `devstack start` / `stop <service>` | one service |
| `devstack run <command>` | a command of the stack file (list below) |
| `devstack top` | CPU, memory, disk per service and container |
| `devstack ui` | dashboard in the browser (pairs the browser once) |
| `devstack down` | stop the services and the daemon; containers keep running. `down --infra` removes them too |

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

`devstack mcp` is an MCP server (stdio) with `status`, `up`, `down`,
`start_service`, `stop_service`, `restart_service`, `promote_service`,
`rollback_service`, `logs`, `diagnostics`, `run_command`, `usage`, `hub_status`, `hub_start`.
Register it once (project `.mcp.json` or user configuration):

```json
{ "mcpServers": { "devstack": { "command": "devstack", "args": ["mcp"] } } }
```

`run_command` for a command with a confirmation text runs only with
`confirm: true`, and that comes from the user, never from the agent.

## 6. When something goes wrong

| Symptom | Cause and fix |
| --- | --- |
| something happened and nothing says why (a restart, a switch, a stop) | `devstack logs --daemon --grep <service>` (MCP `diagnostics`): service and candidate states with reasons, every request that changed something, daemon start and stop; it works with the daemon stopped |
| `devstack up`: Docker is not running | `up` starts Docker Desktop and waits; with Docker Engine as a Linux service it names `sudo systemctl start docker` for the user |
| a port is taken | another process (often a second copy of the stack, an IDE run, a leftover process) holds it; find it (`Get-NetTCPConnection -LocalPort <p>` / `ss -ltnp`) and ask before ending it |
| `failed` candidate | the old build keeps running; `devstack status` shows the reason, the build log is in `.devstack/logs/<service>/build-<id>.log` |
| `held` candidate | waits for `devstack promote <service>` after the user agreed |
| `devstack status`: "the stack file changed since the daemon started" | the daemon reads the file once: `devstack down` + `devstack up` (ask first if others use the stack) |
| a service exits right after start | `devstack logs <service> -n 200`; every start has its own log in `.devstack/logs/<service>/run-*.log` |
| dashboard: "hub not reachable" | `devstack ui` (or the "Start hub" button once `devstack hub install` ran) |
| this skill says it is outdated | `devstack skills install` refreshes it from the installed devstack and this stack file |

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
