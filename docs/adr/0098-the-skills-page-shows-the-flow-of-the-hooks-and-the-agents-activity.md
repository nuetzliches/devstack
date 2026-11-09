# ADR-0098: The Skills page shows the flow of the hooks and the agents' activity; devstack keeps only what happened, on this machine, for a week

- **Status:** Accepted (card in the pull request); built; its activity list moved to a lane's timeline ([ADR-0099](0099-a-lane-shows-its-agents-sessions-in-time-from-their-transcripts-when-switched-on.md))
- **Date:** 2026-11-06
- **Deciders:** project owner
- **Related:** [ADR-0094](0094-a-skill-carries-hooks-that-give-text-to-the-agents.md) (hooks, and its addenda: `lane-stop`, `session-end`, an upstream set up in the agent), [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (upstreams through `devstack mcp`), [ADR-0037](0037-pool-telemetry.md) (the pool's telemetry, which this is not part of); issue #482

## Context

A developer could not see what devstack does for their coding agent while
it works. The Skills page listed the skills and how many hooks each carries,
not when a hook gives its text, under which condition, or what followed.
A hook that stayed silent (an upstream not accepted, #473) said nothing
about why. devstack sees part of what an agent does: the hooks it gives, and
the tool calls through `devstack mcp` (its own tools and the upstreams').
The agent's built-in tools (shell, editing, reading files) do not pass it.

## Decision

The owner approved the review card in the conversation on **2026-11-06**,
all four points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | The activity holds only what happened: the event, the skill, the hook's place or the tool's name, how long it took and whether it failed; never a prompt, a hook's text, a tool's arguments or its answer. It stays on this machine (not in the pool's telemetry) and goes after seven days. | Recording arguments and answers (a full trace); sending it to the pool | Low: a record could gain fields later, with a card of its own |
| 2 | concept | A session is the agent's `session_id` from a hook's input; a tool call (and a lane's stop) carries none and is put to the latest session that started in its working tree before it. | Asking the agent for a session id on the MCP connection (none known to be passed) | Medium: two agents in one working tree at once are not told apart |
| 3 | contract | `GET /hub/skills` carries per hook its event, condition and the start of its text (`hook_details`); `GET /hub/activity` gives the sessions and the records; `/hub/events` sends `activity` when a record was added. | Working the flow out in the browser from the archives | Low: additive fields and a new endpoint |
| 4 | term | The Skills page has a tab **Flow**, beside the sets. | A tab on a working tree's page ("what my agent does here") | Low: that can come as well |

As built:

1. **The records.** `devstack hook` writes one line per session start (to
   know the session), per prompt it gave text for, per `session-end` it kept,
   per `lane-stop` text given; `devstack mcp` one per `tools/call` (tool,
   MCP client name, working tree from its folder, pid, ms, ok). Each process
   appends to its own `activity/<day>-<pid>.jsonl` in the hub's state, so no
   two processes write one file; an error goes to the hub's log, never to
   the hook or the call. Files older than seven days go when the hub reads
   them.
2. **The answer.** `GET /hub/activity?tree=&session=&since=` reads the files,
   builds the sessions from the hooks' records, puts each record without a
   session to the latest session started in its tree before it, and names
   for an upstream's tool the upstream (by the prefix the pool gives it) and
   the skills whose hooks name that upstream. At most the latest 500 records.
3. **The event.** While a dashboard listens, the hub looks at the folder
   every two seconds and sends `{"type":"activity","at"}` when it changed;
   the page reads `/hub/activity` again.
4. **The details.** Per hook of a skill switched on: `index`, `event`,
   `pattern`, `once`, `upstream`, the first 160 characters of its text, the
   agents that get it, and whether its upstream is there (accepted, or set
   up in an agent), with why not.
5. **The page.** The flow's phases (a session starts, a prompt matches, a
   lane stops, a session ends) with each hook as a node in its skill's
   colour; a tooltip says when, on what condition, how often, for which
   agents, and the start of its text; a hook that is not given is greyed
   with why. Beside it the activity, narrowed by a session and by a click on
   a node; below the flow the tool calls by the skill whose upstream they
   use.

Decided while building, not on the card:

- A prompt's hooks are grouped by their pattern within one phase, not split
  into phases by guessing what a pattern means.
- A session start is recorded even when it gave no text: without it the
  session and its tool calls could not be told.
- The record's times are Unix milliseconds as 64-bit numbers (a tagged
  record cannot read a 128-bit one back).

## Alternatives considered

- **A full trace with arguments and answers.** It would show more, but it
  holds what a user typed, file contents and tokens an upstream returned;
  a log of that is a second copy of the user's work outside their control.
- **The agent's built-in tools through a `PostToolUse` hook.** The only way
  to see the shell and the edits; its input carries commands and file
  contents. Left for a card of its own, opt-in, names and durations only.
- **A tab on a working tree's page.** Where a developer looks for "what my
  agent does here"; the Skills page first, because the hooks are the
  skills'.

## Consequences

- Every session start and every tool call through `devstack mcp` writes a
  line to disk; a busy week is a few thousand lines.
- Two agents working in one working tree at once share their tool calls in
  the activity.
- What an agent does without devstack (its own shell, edits) is not shown.

## Enforced by

`crates/devstack-hub/src/activity.rs` (the record, the files, the view, the
event), `crates/devstack-hub/src/agenthooks.rs` (`record`),
`crates/devstack/src/mcp.rs` (`record`), `crates/devstack-hub/src/lib.rs`
(`activity_get`, the `activity` event), `crates/devstack-hub/src/poolskills.rs`
(`hook_details`), `ui/src/lib/flow.ts`, `ui/src/components/SkillsFlow.vue`,
`ui/src/views/SkillsView.vue`, `docs/dashboard-api.md`.

## Addendum 2026-11-09: the activity list moved to a lane

[ADR-0099](0099-a-lane-shows-its-agents-sessions-in-time-from-their-transcripts-when-switched-on.md)
shows a session in time on a lane's Activity (view Timeline), with the
hooks and tool calls recorded here. The list beside the flow went there;
the flow keeps, per hook, when it was given last, and the tool calls by
skill. A hook's record now also keeps the path of the session's transcript
(never its content), left out of `GET /hub/activity`.
