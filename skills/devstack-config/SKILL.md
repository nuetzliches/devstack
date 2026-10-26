---
name: devstack-config
description: "Edits this repository's devstack stack file ({{devstack:stack_file}}): adding or changing a service, a Compose group, a command or a group; watch paths, health checks, background workers, promotion holds, environment from Compose, placeholders. Validates every change with `devstack check` and knows that a running daemon does not reload the file. Use it whenever {{devstack:stack_file}} is created or touched, e.g. \"add the worker to devstack\", \"devstack should restart X when Y changes\", \"add a seed command\", \"the service needs variable Z\", \"why does the candidate not switch\", \"set up devstack for this repo\". For running the stack, use the skill `devstack`."
---

<!-- {{devstack:marker}} -->

# devstack-config: change the stack file

The reference for every key is **[docs/stack-file.md]({{devstack:docs_url}}/stack-file.md)**
(this skill names the rules that are easy to get wrong; the reference has the
details). A complete example: [examples/devstack.toml]({{devstack:repo_url}}/blob/main/examples/devstack.toml).

## Workflow

1. Read the current `{{devstack:stack_file}}` and, for the part you change, the
   matching section of the reference.
2. Change the file. Keep its order and comment style; say *why* in a comment
   when a value is not obvious (a timeout, an ignored variable, a hold).
3. `devstack check` (and `devstack check --compose` when a Compose group, a
   `compose = {…}` reference or an env file changed). Fix until it is clean;
   it reports every problem at once.
4. If you used a key newer than `[stack] requires`, raise `requires`.
5. A running daemon does **not** reload the file. Tell the user it applies with
   `devstack down` + `devstack up`, and ask before restarting a stack others may
   be using. `devstack status` shows "the stack file changed" until then.
6. After the restart, verify the change itself (`devstack status`, the service's
   log, the health URL), not only that the stack came up.

`devstack schema` prints the JSON Schema of the file for editors. A repository
without a stack file starts from `devstack init` (a draft with TODOs).

## Rules that are easy to get wrong

- **Unknown keys are errors.** No key is ignored silently; read the error, do
  not work around it.
- **`watch` vs `restart_on`.** Built services (`build` + `run`) get `watch`: a
  change becomes a candidate that replaces the running instance once healthy.
  Dev servers (`kind = "devserver"`) reload themselves and get `restart_on`
  only for files their hot reload ignores (`tsconfig.json`, `angular.json`).
  In `restart_on` a folder (`src/api`) covers the files in it; in `watch`
  write `src/Api/**`, a bare folder matches no file there. A pattern that names only file types (`src/**/*.cs`) misses a folder that is
  moved into place; `src/Api/**` sees it.
- **Health should mean ready.** devstack switches traffic to a candidate when
  `health` answers 2xx. Point it at a readiness endpoint that checks what the
  service needs (its database), not at a bare liveness probe; the timeout covers
  a cold start.
- **Background workers run once.** While a candidate starts beside the main
  instance, only one of them may run schedulers and queue consumers: put their
  switches into `[services.<name>.workers] env = { X = "{is_main}" }`, not into
  `env`.
- **Lanes have their own containers.** A worktree runs as a lane with its
  ports moved (`[lanes] port_step`) and its own Compose project, so its
  database is its own and needs no `{lane_suffix}`; services that hold nothing
  per lane (a telemetry dashboard, a fake SMTP server) can stay shared:
  `[compose.<group>] shared = ["mailbox"]`. A dev server needs `{port}`
  (in `run`, or in `env` for a config that reads it) to run in a lane. A
  command names a Compose project as `{compose.<group>.project}`, never
  literally (`docker compose -p {compose.app.project} exec -T db ...`): in a
  lane that is the lane's own project, not the main tree's.
- **A lane's worktree has no `node_modules`**: the lane runs from a copy,
  where `prepare` installs (`devstack check` warns about a dev server with
  a `package.json` and no `prepare`). When git hooks need it (`npx eslint` in a
  pre-commit hook), `prepare.link = ["web/node_modules"]` links it into the
  worktree; `.gitignore` then needs `node_modules` without a trailing slash.
- **Lanes on other machines** (`devstack up --on <node>`) run in a Linux
  container: `[lanes] image` (a toolchain image) or `[lanes] dockerfile`. Files
  git ignores do not go there unless `[lanes] sync` names them (a dev
  certificate); `prepare` installs what the copy lacks (`npm ci`).
  `[lanes] share` says whom such a lane is shared with by default. The pool
  places a lane by what its runs took there (ADR-0049); `[lanes.need]`
  (`cpus`, `memory = "6G"`, `disk = "25G"`, `gpus = { count, memory }`) is a
  floor only: set it where the first runs would be placed too small, or for
  a GPU. A profile's `need` raises it for that profile.
- **Agent skills go where the repository's agents look.** `[agents] skills`
  names the folders `devstack skills install` writes to: `.claude/skills`
  (Claude Code, the default), `.agents/skills` (Codex, Gemini CLI and most
  others); both only when both kinds of agents work here, since an agent
  that reads both sees each skill twice.
- **A purpose gets a profile, not a second stack file** (ADR-0041).
  `[profiles.<name>]` changes how the stack runs for an acceptance run or a
  light review: `workers`, `data = "fresh"`, `env` with the tree's
  addresses, `autostart`, per service `kind = "static"` with `build` and
  `output`, and the suite as `command`. Each profile is checked as the stack
  it makes; add `requires = "0.11.0"` to `[stack]`. A light profile for
  daily work leaves heavy containers out with `skip` (ADR-0058):
  `skip = ["compose.search", "compose.app.scanner", "indexer"]` (a whole
  group, a container, a service; `requires = "0.21.0"`). What it skips is
  stopped at the start and still starts on demand (`devstack start
  compose.app.scanner`); a service that starts and uses a skipped
  container's address (`{compose.…}`, `rewrite_hosts`) must be skipped too.
- **Hot reload is opt-in per session.** `[services.<name>.hot_reload]` with
  the watcher's `run` (`dotnet watch run --non-interactive`) only makes
  `devstack hot-reload <name> on` possible; the service still builds by
  default. Built services only.
- **Schema changes wait.** Paths that mean "this build needs a migration" go
  into `promote = { hold_if_changed = [...] }`; such a candidate waits for
  `devstack promote` instead of switching.
- **Environment from Compose.** `compose = { group, service }` gives the native
  process the container's environment with host names rewritten
  (`rewrite_hosts`). When the Compose service describes a *deployment* rather
  than this dev process, use `inherit = false` with an `ignore` list: then
  `devstack check --compose` reports every new variable of the deployment the
  stack file does not handle.
- **Ports.** `port` is the fixed port users and other services see. Behind the
  proxy the process listens on `{port}` (an internal one), so its listen address
  must come from `{port}`, never from the fixed number.
- **Container links** in the dashboard default to `http://localhost:<port>` per
  published port; set `urls = { <service> = "https://localhost:9443/ui" }` (or
  `""` for none) in the Compose group where that is wrong.
- **Main ports for containers.** `promote --lane` moves the fixed ports of
  `[services.*]` only, unless the Compose group names containers in
  `main_ports = ["web", "api"]`: devstack then holds their published TCP
  ports and points them at the lane. Name what the developer reaches directly
  from the host (UI, the API the browser calls), not what those use.
- **Ports a server announces** (a WebRTC media server's ICE candidates, a
  broker's advertised listeners) go into the Compose group's
  `announced_ports = ["media:7881", "media:7882/udp"]`, each published 1:1
  (`7881:7881`). A lane then moves them inside the container too, and
  Compose (and every `exec` step) gets `DEVSTACK_PORT_MEDIA_7881` with this
  tree's number: pass it to the server's listen and announce settings with
  the files' port as default (`${DEVSTACK_PORT_MEDIA_7881:-7881}`); keep
  `ports` literal. Without the key, a lane's server announces the main
  tree's port. Add `requires = "0.20.0"` to `[stack]`.
- **A per-lane value in a container** (a login cookie's name, which the
  browser shares between the main tree and every lane on `localhost`): every
  `docker compose` call (and every `exec` step) gets `DEVSTACK_LANE`
  (`main` in the main tree) and `DEVSTACK_LANE_SUFFIX` (empty there,
  `_<lane>` in a lane). Read them in the Compose file with a default
  (`REFRESH_COOKIE: app_refresh${DEVSTACK_LANE_SUFFIX:-}`), never from
  `COMPOSE_PROJECT_NAME`. For a native service use `{lane_suffix}` in its
  `env`. Both give a lane on a pool node its own name too.
- **A container as a service** (`run = "docker run ..."`, for what needs
  Linux tools but should get `watch` and candidates) needs three things: a
  process in it that ends on `SIGINT` and `SIGTERM` (`docker run --init`,
  or an entrypoint that handles them: devstack's graceful stop reaches the
  container through the CLI), `--rm`, and a name with
  `{lane_suffix}-{port}` so a candidate and the main instance do not
  collide. Add `stop = "docker rm -f <the same name>"`: devstack runs it
  whenever an instance ends, also after a crash, so no container is left
  holding the port. Add `requires = "0.21.0"` to `[stack]`.
- **Paths** are relative to the repository root; use `{repo}` where a process
  needs an absolute one. Generated directories (`bin`, `obj`, `node_modules`,
  `target`, `.git`, `.devstack`) never count as changes. Compose files, env
  files and `[lanes] dockerfile` must stay inside the repository (no absolute
  path, no `..`).
- **Placeholders only where they have a value**: `{port}` needs `port`,
  `{artifact}` needs `build`; `build` and `output` know only `{repo}` and
  `${VAR}`; `cwd` and `prepare` take none. `devstack check` names each.
- **Jobs end**: a `kind = "job"` counts as failed after 30 minutes unless it
  sets `timeout = "…"`; give a long seed or migration one.
- **Commands with side effects** (deleting data, resetting state) get a
  `confirm` text. Steps run in order; one action per step.
- **Secrets** do not belong in the stack file. Use `${VAR}` and an env file that
  is not committed (`stack.env_files`), with a committed `.env.example` beside it.

## When the stack file is not enough

If devstack lacks something this repository needs (a key, a placeholder, a
behaviour), do not bend the file around it: ask the user for permission to open
an issue in `nuetzliches/devstack` (see the skill `devstack`, section 7, and
keep the project's names out of it).
