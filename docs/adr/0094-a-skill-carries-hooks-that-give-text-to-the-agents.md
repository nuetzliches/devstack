# ADR-0094: A skill carries hooks that give text to the agents; the hub writes one entry per agent and event, `devstack hook` picks the texts

- **Status:** Accepted (card in the pull request); built; addenda 2026-11-06 (end-of-session events, #462; an upstream the agent has set up itself counts, #473)
- **Date:** 2026-11-05
- **Deciders:** project owner
- **Related:** [ADR-0074](0074-a-pool-hands-out-skill-sets-the-hub-installs-them-for-the-agents.md) (skill sets, acceptance, the agent table), [ADR-0077](0077-a-sets-skills-are-grouped-and-switched-one-by-one.md) (a skill switched on and off), [ADR-0079](0079-skills-local-to-a-working-tree.md) (a skill bound to repositories), [ADR-0083](0083-the-hub-takes-out-what-an-agent-switched-off-leaves.md) (an agent switched off leaves nothing), [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (a pool's upstreams, accepted per member), [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md); issue #453

## Context

A skill fires when the model judges that it should. Some routines must
reach every session, or every prompt that names a ticket, whatever the
model decides:

- A time-tracking skill that starts a timer at the start of a session was
  listed but not loaded when a session began with a long step-by-step
  prompt. Its MCP upstream was still connecting and its tools were
  deferred behind a tool search, so the skill's own condition ("if the
  tools are there") was false. The upstream's `instructions`, passed on
  since 0.39.0, are text the model weighs too.
- A repository ships a `UserPromptSubmit` hook that recognises a ticket
  reference in the prompt and injects a checklist, because a bare ticket
  URL as the whole message regularly did not trigger the matching skill.
  It works for Claude Code only, and only where the repository commits it.

Agent harnesses run hooks mechanically. Of ten looked at (2026-11-04,
their documentation): Claude Code and Codex have a session-start and a
prompt-submit event whose hooks can add text to the context, with the same
input (`session_id`, `cwd`, `prompt`, `source`); Gemini CLI, Devin
(Windsurf) and Kiro have both too, with Windows undocumented; Cursor and
Copilot add text at a session's start only; Cline runs on macOS and Linux
only; OpenCode and Amp have code plugins, not commands. No format gives an
entry an id. Codex runs a user's hook only once it was trusted, by its
hash.

## Decision

The owner approved the review card in the conversation on **2026-11-05**,
all six points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | A hook lives in a file `hooks.toml` in its skill's folder and shares the skill's fate: packed, accepted, switched on and off (ADR-0077), bound to repositories (`metadata.repos`) and updated with it. | A manifest per set; hooks as objects of their own beside the skills. | Hooks at the set's level would come additively; taking them out of the skills breaks the sources. |
| 2 | concept | The hub writes one entry per agent and event, `devstack hook <event> --agent <id>`; this dispatcher decides on each call which hooks apply (skill installed and on, `cwd` in a bound repository, upstream accepted, pattern matches, not given yet) and prints their texts. | An entry per hook with its text in the agent's configuration; bound hooks in the working tree's `.claude/settings.local.json`. | Medium: entries per hook would have to be written and tracked on every change. |
| 3 | trust | A pool's hook only adds text to the context; no command of a pool runs on a member's machine. If wrong: a pushed text steers every agent in every session without the model loading a skill, but it passes the same gate as a skill (acceptance, updates). | Command hooks from the pool. | Command hooks could come later, additively, with an acceptance of their own per hook. |
| 4 | contract | The format: `[[hook]]` with `event` (`session-start` or `prompt-submit`), `match` (a regex on the prompt, `prompt-submit` only), `once` (`session` or `match`), `upstream` (optional), `context` or `context_file` (a file in the skill's folder), at most 2000 characters a hook. | YAML in the frontmatter (every agent reads it); the agents' own event names. | The pool's format has users outside the maintainers: only additive changes from now on. |
| 5 | concept | The agent switch of the Skills page applies; hooks go only to an agent with an adapter in `[agent.hooks]` (devstack's own section of `agents.toml`, like `[agent.open]`), first Claude Code and Codex; the Skills page and `doctor` say which agent switched on gets none. | A list of agents of its own for hooks; all ten agents now. | Low: another adapter is additive. |
| 6 | concept | The dispatcher never holds up a prompt: it reads the hub's state from disk (no hub needs to run) and prints nothing on any error, which goes to the diagnostic log. | Asking the hub over HTTP (it may be stopped); an error shown in the prompt. | Low. |

As built:

1. **The format** (`devstack_node::hooks`). `hooks.toml` holds up to eight
   `[[hook]]` tables; an unknown key (a `command`, say) is an error.
   `match` is for `prompt-submit` only and must compile as a regex (500
   characters at most). `once` defaults to `match` with a pattern, else to
   `session`; `once = "match"` without a pattern is an error. `upstream`
   is an upstream's name (`a-z0-9-`, up to 32). The text, from `context` or
   from the skill's file `context_file`, is trimmed and 1 to 2000
   characters.
2. **The fetch.** The coordinator reads `hooks.toml` when it packs a skill;
   a wrong one leaves the skill out, with why, as a wrong `SKILL.md` does.
   The manifest names the number of hooks per skill (`hooks`, left out at
   0; an older coordinator sends none, read as 0).
3. **The index.** Each sync of the pools' skills (ADR-0074) reads the hooks
   of every skill it wants, from the archive of the version the user agreed
   to (with updates waiting, the text stays as agreed to), and writes
   `agent-hooks.json` in the hub's state: each hook with its pool, set,
   skill, place in the file and the skill's repositories; the agents whose
   configuration has the hub's entries; the agents switched on that get
   none. Written only when it changes.
4. **The entries.** `[agent.hooks]` names the file and its format:
   Claude Code `${CLAUDE_CONFIG_DIR:-~/.claude}/settings.json`, Codex
   `${CODEX_HOME:-~/.codex}/hooks.json`, both `hooks.<Event>[].hooks[]`
   (`SessionStart`, `UserPromptSubmit`). The entry is `{"type": "command",
   "command": "devstack hook <event> --agent <id>", "timeout": 5}` in a
   group of its own, without a matcher; the hub knows its entries by that
   command. It writes an entry for each event that has hooks, for each
   agent switched on whose folder exists; it takes them out for an agent
   switched off, an event without hooks, and when no hook is left. The
   file's other keys keep their order and their text (read as raw JSON),
   the user's hooks stay; a file that is not a JSON object is left alone,
   with the error on the Skills page. It is written through a file beside
   it, and only when the entries change. `scripts/sync-agents.py` keeps
   `[agent.hooks]` as it keeps `[agent.open]`.
5. **The dispatcher** (`devstack hook <event> --agent <id>`, hidden). It
   reads the agent's input on stdin (`session_id` or `sessionId`, `cwd`,
   `prompt`, `source`) and the index. An agent the index does not name
   gets nothing. A hook bound to an upstream applies while the member
   accepted that pool's upstream at the URL it has now and has none of
   their own of that name (`upstreams.toml`, `pool-upstreams.json`). A
   bound skill's hook applies where `git remote get-url origin` of `cwd`
   matches its repositories, as ADR-0079 matches them. What a session was
   given is kept per agent and session in `hook-sessions/` (a hash of
   both), for a week; a session that starts again after `clear` or
   `compact` has lost its context and is given the texts again. The texts
   go to stdout, a blank line between them; both agents add stdout to the
   context. The exit code is always 0; an unreadable index or a session
   file that cannot be written goes to the hub's diagnostic log
   (`agent_hook`).
6. **Visibility.** The Skills page shows a skill's hooks (a pill, and in
   its detail the agents that get them) and *no hooks* on an agent
   switched on without an adapter; `GET /hub/skills` carries `hooks` per
   skill and per agent. `devstack doctor` names how many hooks reach which
   agents, notes that Codex runs the entry once trusted with `/hooks`,
   names the agents switched on that get none, and warns when `devstack`
   is not on PATH.

Decided while building, not on the card:

- The entry calls `devstack` from PATH, not the binary's path: a path
  changes with an update or a Homebrew upgrade, and Codex would ask to
  trust the entry again.
- The once-state is kept by the dispatcher in the hub's state, a file per
  agent and session, forgotten after seven days.
- `once` defaults to `match` for a hook with a pattern; for
  `session-start` it means once a session (a resumed session is not given
  the text again).
- The file is `hooks.toml`: devstack's files are TOML.

## Alternatives considered

- **Hooks in the skill's frontmatter** (the issue's proposal). One file
  fewer, but every agent reads the frontmatter, and a text of 2000
  characters there would sit in every agent's skill list.
- **An entry per hook, its text in the agent's configuration.** No process
  per prompt, but every change of a text rewrites the user's settings
  (and has Codex ask for trust again), a pattern cannot be matched by
  Claude Code's prompt event, and a bound hook would need the working
  tree's `.claude/settings.local.json`.
- **Command hooks from the pool.** More power (a hook that asks a tracker),
  but code of the pool on every member's machine without the model or the
  user in between.
- **All agents with hooks now.** Cursor and Copilot add text at a
  session's start only; Gemini, Devin and Kiro are not documented on
  Windows; Cline not there at all. Each can be an adapter later.

## Consequences

- A routine that must not depend on the model's judgement reaches every
  session of Claude Code and Codex, and a prompt naming a ticket gets its
  checklist once per ticket, from one source, for every member.
- Each session start and each prompt starts `devstack` once (a few
  milliseconds; a hook bound to repositories asks git as well).
- A member who uses Codex trusts the entry once (`/hooks`).
- A skill's hook text changes with the skill: with auto-update on, a push
  to the source changes what every agent is told at its next session.
- Not built: adapters for Cursor, Copilot, Gemini, Devin, Kiro; hooks for
  devstack's own generated skills; command hooks.

## Enforced by

`crates/devstack-node/src/hooks.rs` (the format), `crates/devstack-node/src/skills.rs`
(`pack`: the check at the fetch, `SkillEntry::hooks`),
`crates/devstack-hub/src/agenthooks.rs` (the index, `configure`, `run`),
`crates/devstack-hub/src/poolskills.rs` (`sync`: the hooks of the skills
wanted), `crates/devstack-hub/src/upstreams.rs` (`pool_accepted`),
`crates/devstack-skills/agents.toml` (`[agent.hooks]`),
`crates/devstack-skills/src/agents.rs`, `scripts/sync-agents.py`,
`crates/devstack/src/main.rs` (`devstack hook`), `crates/devstack/src/doctor.rs`
(`agent_hooks`), `ui/src/views/SkillsView.vue`.

## Addendum 2026-11-06: two events for the end of a session

Issue #462. A routine that belongs to a session's end (stop a timer, review
the session) had only a `prompt-submit` hook matching closing words: a
session that ends without one gets nothing, a closing word mid-session fires
early. The owner approved the review card in the conversation on
**2026-11-06**, all three points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | `event` gets two values, `lane-stop` and `session-end`; a hub from 0.42.0 skips a hook of an event it does not know instead of leaving out the skill's hooks, a hub before it still leaves them out, so the documentation advises a skill of their own while members run older releases. | A version per hook; a file of their own for the new events. | The format has users outside the maintainers: additive changes only. |
| 2 | concept | `lane-stop`: after `devstack lane stop` (CLI) or the MCP tool `lane_stop` succeeded, devstack appends the texts of the `lane-stop` hooks that apply to the lane's worktree (skill on, repository bound, upstream accepted) to the output, every time, whichever agent ran it. | An agent hook on the shell command (Claude Code's `PostToolUse` only). | Low. |
| 3 | concept | `session-end`: the hub writes an entry for Claude Code's `SessionEnd`; the dispatcher only records the end (agent, session id, transcript path, working tree, time), and the next `session-start` of the same agent in the same working tree gives the texts once, with a line naming the ended session; a record older than seven days is dropped. | Text at `SessionEnd` (it never reaches the model); the closing-words heuristic. | Medium: state on disk, one more agent entry. |

As built:

1. **The format.** `Event` has `session-end` and `lane-stop`; `match` stays
   `prompt-submit`'s. `hooks::of_files` (the hub, reading an archive) skips
   a hook whose `event` looks like an event's name (`a-z` and `-`) but is
   not one it knows; `hooks::check_files` (the coordinator, packing) refuses
   it, so an author's typo still leaves the skill out with why.
2. **The entries.** `configure` writes entries for `session-start`,
   `prompt-submit` and `session-end` where the agent has the event: Claude
   Code `SessionEnd`; Codex documents no end of a session and gets none.
   `lane-stop` is no agent's event; hooks of only that event write no entry.
3. **The end.** `devstack hook session-end` prints nothing. Where a
   `session-end` hook applies at `cwd`, it writes `hook-ended/<hash of agent
   and working tree>.json` in the hub's state (session id, transcript path,
   time). The working tree is the nearest folder holding `.git` above `cwd`
   (no git asked; on Windows compared without case). The next
   `session-start` of that agent there appends, after the session-start
   texts, "The previous session in this working tree (session `…`,
   transcript `…`) ended at …. What follows was meant for its end:" and the
   texts of the `session-end` hooks that apply then, and removes the file.
   Records not touched for seven days are removed.
4. **The lane stop.** `client::lane_stop_texts` looks up the lane's worktree
   (`GET /v1/lanes`, by name or branch) and `agenthooks::texts_here` gives
   the `lane-stop` texts that apply there; `devstack lane stop` prints them
   after its line, `lane_stop` appends them to its result. An error gives
   nothing; the stop stands.
5. **Visibility.** `devstack doctor` names the agents that get hooks but no
   `session-end` ones.

Decided while building, not on the card:

- `lane-stop` gives its texts on every stop; `once` does not apply.
- The end is recorded only where a `session-end` hook applies; a record is
  given whatever the next session's source (`startup`, `resume`, `clear`).
- The coordinator stays strict about events (`check_files`).

*Enforced by, added:* `crates/devstack-node/src/hooks.rs` (`Event`,
`of_files`, `check_files`), `crates/devstack-hub/src/agenthooks.rs`
(`event_name`, `end`, `ended`, `texts_for`), `crates/devstack/src/client.rs`
(`lane_stop`, `lane_stop_texts`), `crates/devstack/src/mcp.rs` (`lane_stop`),
`crates/devstack/src/doctor.rs` (`agent_hooks`).

## Addendum 2026-11-06: an upstream the agent has set up itself counts

Point 5 of the dispatcher (*a hook bound to an upstream applies while the
member accepted that pool's upstream*) left a hook silent for a member who
had set the upstream up in the agent directly, outside devstack (#473). The
owner approved in the conversation on **2026-11-06** (point 5 of the pull
request's card; points 4 and 6 are in ADR-0069's addendum of the same day):

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 5 | concept | A hook bound to an upstream also applies when the agent has a server of its own whose URL (normalised: scheme, lower-case host, port, path without trailing `/`) is the pool's upstream's URL: a direct configuration counts as the upstream being there. | Only a note line instead of silence. | Low. |

As built: `upstream_there` in `crates/devstack-hub/src/agenthooks.rs` takes
an accepted upstream as before, and only when it is not accepted reads the
servers of the agent the entry names (`--agent`: Claude Code's user scope,
its local scope for the session's `cwd`, the `.mcp.json` there; Codex's
`config.toml`; `crates/devstack-hub/src/agentmcp.rs`) and compares their
URLs with the one the pool's list gives (`pool-upstreams.json`). A member's
own hub upstream of that name still keeps the pool's out.
