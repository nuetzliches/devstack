# ADR-0100: The user can register devstack's MCP server for every folder; the hub writes only its own entry `devstack`

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-09
- **Deciders:** project owner
- **Related:** [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (`devstack mcp` carries the upstreams), [ADR-0094](0094-a-skill-carries-hooks-that-give-text-to-the-agents.md) (the hub writes the agents' hook entries, by the same rules), issue #492

## Context

`devstack mcp` carries the pool's upstream tools and the hooks bound to
them, but an agent got it only where a repository's own file registered
it: the dashboard offered that entry in a stack's head, so a repository
without a stack file never had it. An agent session there had no devstack
tools, every accepted upstream was missing, hooks bound to them stayed
silent, and nothing said why. The server works in any folder; only the
registration was missing. The hub read the agents' MCP configuration and
never wrote it (card of 2026-11-06, point 4).

## Decision

The owner approved the review card in the conversation on **2026-11-09**,
all five points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | For the agents the user picks, the hub writes one entry `devstack` into the agent's user configuration (`~/.claude.json` `mcpServers`, `~/.codex/config.toml` `[mcp_servers.devstack]`, `~/.cursor/mcp.json`); only that entry, the rest of the file kept, removed again when switched off, by ADR-0094's rules for the hooks. | A hint the user follows by hand (`claude mcp add -s user …`) | Low: switching off removes it |
| 2 | term | A setting in the hub per machine and agent: devstack's MCP server **per repository** (today, the default) or **for me, in every folder**; the user decides, not a pool or a stack file. | A pool pushing the registration | Low |
| 3 | concept | The user entry is always named `devstack`: a project's entry of that name is the same server and loads once; an entry that runs `devstack mcp` under another name is reported as a double by `doctor` and the dashboard, not removed. | `devstack mcp` noticing a sibling at runtime (it cannot see the agent's configuration) | Medium |
| 4 | contract | `devstack doctor` (and the MCP tool `doctor`) says in a folder without a stack file too whether devstack's MCP server is registered there and how to do it; the dashboard's hint offers every folder. | `doctor` inside a stack only | Low |
| 5 | contract | `GET`/`PUT /hub/mcp/scope` with `{agents: {<id>: "repository" \| "user"}}`. | — | Low: a new endpoint |

As built (`crates/devstack-hub/src/mcpscope.rs`):

1. **The entry.** `{"type": "stdio", "command": "devstack", "args": ["mcp"]}`
   in JSON, `command`/`args` in Codex's table: the command line of the
   repository entry. A file that is not there is made.
2. **Only its own.** JSON: the top-level keys stay in their order, every
   value but `mcpServers` as it was written (the hooks' technique); Codex:
   `toml_edit`, so comments and layout stay. An entry `devstack` that runs
   `devstack mcp` and was there before is the user's own: left on, not
   removed when switched back (the setting keeps which entries the hub
   wrote, `written`). One that runs something else is an error, and the
   file is not changed. Removing takes only an entry that still runs
   `devstack mcp`; an emptied `mcpServers` goes.
3. **The setting.** `mcp-scope.json` in the hub's state: the agents set to
   `user` and the ones whose entry the hub wrote. The page lists Claude
   Code, Codex and Cursor with their file, whether the agent is installed
   (its folder is there; not installed, "in every folder" is off), whether
   its user configuration registers the server already, and an error.
4. **Doubles** (`mcp::doubles`): per agent (Claude Code: `.mcp.json`,
   `~/.claude.json` and its project's entries; VS Code; Cursor; Codex), when
   its files run `devstack mcp` under more than one name, every one not
   named `devstack`. `GET /hub/stacks/{id}/mcp` carries them (`doubles`),
   the stack's head shows "MCP twice", `doctor` warns.
5. **Where it shows.** A card on the MCP upstreams page; the stack's "no
   MCP" hint links there; `doctor` reports for the stack's repository, or
   for the folder it runs in without a stack file.

Decided while building, not on the card:

- VS Code has no user file the hub writes (its user MCP configuration
  lives in its settings profile); it keeps the per-repository prompt.
- Whether Cursor merges a user and a project entry of the same name is not
  checked: `doctor` and the dashboard report only different names.

## Alternatives considered

- **Writing the entry under the repository's own name** to avoid a double.
  A name differs per repository; one fixed name lets the agents' own
  precedence (local over project over user) pick one.
- **A pool policy that registers it for its members.** The registration is
  the user's tooling, not the pool's; a pool cannot see what else the
  user's agents run.

## Consequences

- Claude Code rewrites `~/.claude.json` often; the hub writes through a
  file beside it and renames, so a write that crosses one of Claude Code's
  can lose one of the two (switching again repairs ours).
- A repository's entry under another name than `devstack` now shows as
  "MCP twice" where a user entry exists.

## Enforced by

`crates/devstack-hub/src/mcpscope.rs`, `crates/devstack-hub/src/mcp.rs`
(`doubles`), `crates/devstack-hub/src/lib.rs`
(`mcp_scope_get`, `mcp_scope_put`, `stack_mcp`), `crates/devstack/src/doctor.rs`
(`mcp_registration`), `ui/src/components/McpScope.vue`,
`ui/src/components/McpHint.vue`, `docs/dashboard-api.md`.
