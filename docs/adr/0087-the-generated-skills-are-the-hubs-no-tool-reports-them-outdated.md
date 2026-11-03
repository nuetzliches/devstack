# ADR-0087: The generated skills are the hub's to keep current; no tool reports them outdated

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0079](0079-skills-local-to-a-working-tree.md) (the hub writes devstack's own skills into the working trees), [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md) (the `skills` prompt and the banner, removed here), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (the prompt kinds), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

Since ADR-0079 the hub writes devstack's generated skills (`devstack`,
`devstack-config`) into every registered working tree itself: its
`tree-skills` thread looks at a print of the stack files, the worktrees and
their branches every 3 seconds and writes the skills again when it changes,
and once a minute at the latest. A new devstack restarts the hub, which
writes them again too.

The dashboard still showed a banner, "The agent skills no longer match the
stack file", with a prompt that had a coding agent run
`devstack skills install`; `devstack status`, the MCP `status`,
`devstack doctor` and `devstack skills check` reported the same. The
dashboard read the state when a stack was opened, when the hub's version
changed and when `config_changed` flipped: an edit of the stack file was
read in the seconds before the hub had written the skills again, and the
banner kept that moment until the page was reloaded. The skills on disk
were current; the agent the prompt started would have done what the hub
had already done.

What the hub leaves alone is a generated skill git still tracks (ADR-0079:
devstack does not write over a tracked file), a copy an earlier devstack
had committed. That copy is outdated for good, and no refresh changes that:
it has to come out of git.

## Decision

The owner approved the points in the conversation on **2026-11-03**, all
four:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | devstack's generated skills are the hub's: it writes them again within seconds of a change of the stack file, the branch or the version (ADR-0079); no tool reports them as outdated any more. | A banner that catches the seconds between the change and the hub's write, and has an agent do what the hub has done. | Low: the banner comes back from git's history. |
| 2 | contract | `GET /hub/stacks/{id}/skills` and the prompt kind `skills` are removed. | An endpoint and a prompt nobody needs. | Low: the hub serves the dashboard, so no older client asks for them. |
| 3 | contract | `devstack status`, the MCP `status` and `devstack doctor` no longer print "no longer match". | A note that is wrong in the same window. | Low. |
| 4 | contract | `devstack skills check` checks the one real gap: generated skills git still tracks, which the hub skips; then it exits 1 and prints `git rm -r --cached …`, as `skills install` does. | Removing the command (scripts break), or a check that never fails. | Low: only the reason for the exit code changes. |

As built:

1. **The library.** `devstack_skills::Installed` keeps `found` and `from`
   (the upgrade's `from` for a stack file without `[stack] reviewed`) and
   gains `folders`, the same skills as paths; `outdated`, `outdated()` and
   `outdated_note()` are gone, and `installed()` no longer renders the
   templates to compare.
2. **The hub.** The route, `skills_view` and `Prompt::Skills` are gone;
   `GET /hub/prompt?kind=skills` answers `400` like any unknown kind. The
   upgrade prompt still has the agent run `devstack skills install` and
   take tracked copies out of git.
3. **The dashboard.** `SkillsBanner.vue`, the store's `skills` and
   `loadSkills`, `skillsState` and the `skills` prompt kind are gone; the
   "stack file behind" banner no longer adds which devstack the skills
   came from.
4. **`skills check`.** Prints `skills: none tracked by git` and exits 0, or
   lists the tracked folders with `git rm -r --cached "<folder>"` and exits
   1.

## Consequences

- Nothing in devstack tells a user or an agent that the generated skills
  are behind; a skill an agent reads in the seconds before the hub writes
  it again may name the old services. The next session reads the new ones.
- A repository that still commits generated skills is found by
  `devstack skills check` (and the upgrade prompt), not by the dashboard.
- Without a hub running, nothing refreshes the skills; `devstack skills
  install` still does on request.

## Enforced by

`crates/devstack-skills/src/lib.rs` (`installed`), `crates/devstack/src/dispatch/skills.rs`
(`skills check`), `crates/devstack-hub/src/lib.rs` (no `/skills` route under a
stack, no `skills` prompt), the hub's tests in `crates/devstack/tests/hub.rs`
(`POST` there `404`/`405`, the prompt `400`).
