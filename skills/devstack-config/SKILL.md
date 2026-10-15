---
name: devstack-config
description: Edits this repository's devstack stack file ({{devstack:stack_file}}): adding or changing a service, a Compose group, a command or a group; watch paths, health checks, background workers, promotion holds, environment from Compose, placeholders. Validates every change with `devstack check` and knows that a running daemon does not reload the file. Use it whenever {{devstack:stack_file}} is created or touched, e.g. "add the worker to devstack", "devstack should restart X when Y changes", "add a seed command", "the service needs variable Z", "why does the candidate not switch", "set up devstack for this repo". For running the stack, use the skill `devstack`.
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
- **Health should mean ready.** devstack switches traffic to a candidate when
  `health` answers 2xx. Point it at a readiness endpoint that checks what the
  service needs (its database), not at a bare liveness probe; the timeout covers
  a cold start.
- **Background workers run once.** While a candidate starts beside the main
  instance, only one of them may run schedulers and queue consumers: put their
  switches into `[services.<name>.workers] env = { X = "{is_main}" }`, not into
  `env`.
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
- **Paths** are relative to the repository root; use `{repo}` where a process
  needs an absolute one. Generated directories (`bin`, `obj`, `node_modules`,
  `target`, `.git`, `.devstack`) never count as changes.
- **Commands with side effects** (deleting data, resetting state) get a
  `confirm` text. Steps run in order; one action per step.
- **Secrets** do not belong in the stack file. Use `${VAR}` and an env file that
  is not committed (`stack.env_files`), with a committed `.env.example` beside it.

## When the stack file is not enough

If devstack lacks something this repository needs (a key, a placeholder, a
behaviour), do not bend the file around it: ask the user for permission to open
an issue in `nuetzliches/devstack` (see the skill `devstack`, section 7, and
keep the project's names out of it).
