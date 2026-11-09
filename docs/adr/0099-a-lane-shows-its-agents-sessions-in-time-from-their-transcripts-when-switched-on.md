# ADR-0099: A lane shows its agents' sessions in time; the hub reads their transcripts when switched on, and takes only times, counts and names

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-09
- **Deciders:** project owner
- **Related:** [ADR-0098](0098-the-skills-page-shows-the-flow-of-the-hooks-and-the-agents-activity.md) (the activity this extends; its list moves here), [ADR-0094](0094-a-skill-carries-hooks-that-give-text-to-the-agents.md) (the hooks whose input names the transcript)

## Context

A developer asked how many tokens their agent had used by the time of a
tool call, and how long the calls took, along the session. devstack's own
records ([ADR-0098](0098-the-skills-page-shows-the-flow-of-the-hooks-and-the-agents-activity.md))
cannot say: an MCP call carries no token counts, and the agent's own tools
(shell, edits, reads) never pass devstack. The agent itself writes both
down, in its transcript: Claude Code a JSONL with each answer's `usage`
and each `tool_use` with its `tool_result`, Codex a rollout with
`token_count` events and its calls. Each agent's hook input names the file
(`transcript_path`). There is no common format.

## Decision

The owner approved the review card in the conversation on **2026-11-09**,
all five points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | For each agent a reader of its own reads that agent's transcript, Claude Code and Codex first; it takes only times, the model's name, token counts, tool names and durations, never a prompt, a text or an argument, and keeps nothing. | A `PostToolUse` hook that records each call (its input holds commands and file contents) | Low: switched off, the activity stays as before |
| 2 | trust | Reading transcripts is a setting per machine, off by default. | On by default | Low |
| 3 | concept | A session gets a timeline of tool calls (the agent's own too) and tokens summed up, from the transcript, not from the activity's files. | Token counts on devstack's MCP records (which have none) | Medium |
| 4 | contract | `GET /hub/activity/timeline?session=&agent=` answers the calls (start, duration, tool, ok), the turns with their tokens and the marks; `GET`/`PUT /hub/activity/transcripts` the setting. | Parsing the transcript in the browser | Low: new endpoints |
| 5 | term | The timeline is a view of a lane's Activity (Journal, Timeline), with a choice of session; the activity list moves there from the Skills page, the flow stays. | A third tab on the Skills page | Low |

As built:

1. **The path.** A hook's record keeps the `transcript_path` of its input
   (`transcript`), the path only; `GET /hub/activity` leaves it out.
2. **The readers** (`crates/devstack-hub/src/transcript.rs`). Claude Code:
   one turn per `message.id` (an answer comes as one line per part), its
   time the first line's, its usage the largest seen; a call from its
   `tool_use` to its `tool_result` (`is_error`: failed); `mcp__<server>__<tool>`
   an MCP server's, `Task`/`Agent` a sub-agent; a sub-agent's turns
   (`isSidechain`, or `<session>/subagents/*.jsonl`) count, its calls do not
   show; a user line with text a prompt, unless the agent added it
   (`isMeta`, a summary, `origin`: a background task's notice, a local
   command); `[Request interrupted` an interruption; `compact_boundary` a
   compaction; `isApiErrorMessage` an error. Codex: a turn per
   `token_count` whose total moved (it comes twice), cached input read from
   the cache; `user_message` a prompt (`task_started` in a rollout without
   one); `function_call`/`custom_tool_call` to its output; an MCP tool once,
   from `mcp_tool_call_end` with its server and duration; `turn_aborted` an
   interruption. The text a reader looks at to tell a prompt from the rest
   is read, never kept.
3. **Through devstack or not.** An MCP call went through devstack when its
   server is named `devstack`, or one of devstack's records has the same
   tool within two seconds; then it is devstack's own tool or an upstream's
   by its prefix, like the activity.
4. **A file read once per change.** The hub keeps what it read of a file
   until its size or time changes (at most sixteen files); a transcript of
   47 MB reads in about 130 ms.
5. **The view.** Rows: prompts (numbered, no text), the model's turns (as
   tall as their output), the calls by where they went (devstack, upstreams,
   other MCP servers, built-in, sub-agents; overlapping calls one below the
   other, a failed one red), what changed the lane (its journal and the
   daemon's events), the hooks given. Below, the context per turn (what the
   model read in: a compaction shows as a drop) and the tokens summed up,
   stacked (cache read, written, input, output; cache read starts hidden,
   it dwarfs the rest). A pause longer than two minutes is pushed together.
   Hovering a call says how long it took and the tokens until then; a
   crosshair reads the charts. Zoom 1× to 8×. A running session is read
   again every five seconds.

Decided while building, not on the card:

- The Skills page keeps when each hook was given last and the tool calls by
  skill; clicking a hook no longer narrows a list (the list moved).
- `GET /hub/activity?tree=` takes a tree as the page knows it (any case,
  any separator) and narrows the sessions too.

## Alternatives considered

- **A `PostToolUse` hook per call.** Would see the agent's own tools
  without reading a transcript, but its input carries the command and the
  file contents, it runs a process per call, and it still has no tokens.
- **Parsing in the browser.** The dashboard would need the file, and with
  it everything in it.
- **Keeping a copy of the counts.** Not needed: the agent keeps its
  transcript, and a timeline read when asked cannot outlive the user's own
  file.

## Consequences

- A session started before this version has no path recorded: its timeline
  says the transcript was not found and shows devstack's records.
- Whether Codex's hook input names `transcript_path` is not yet seen in a
  real session; without it a Codex session shows devstack's records.
- An agent with a new transcript format needs its reader changed; until
  then it shows fewer turns or calls, never more than the file holds.

## Enforced by

`crates/devstack-hub/src/transcript.rs` (the setting, the readers, the
timeline), `crates/devstack-hub/src/activity.rs` (`transcript` on a hook's
record, left out of `/hub/activity`), `crates/devstack-hub/src/agenthooks.rs`
(`record`), `crates/devstack-hub/src/lib.rs` (`timeline_get`,
`transcripts_get`, `transcripts_put`), `ui/src/lib/timeline.ts`,
`ui/src/components/SessionTimeline.vue`, `ui/src/components/AgentTimeline.vue`,
`ui/src/views/EventsView.vue`, `docs/dashboard-api.md`.
