# ADR-0071: The dashboard writes no file of a repository; a change to one is a prompt for a coding agent

- **Status:** Accepted (2026-10-27)
- **Date:** 2026-10-27
- **Deciders:** project owner
- **Related:** [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (prompts for an agent; the `skills` prompt added here), [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) (worktrees, unchanged), [ADR-0055](0055-a-lane-without-a-stack-file-says-so-and-can-be-given-one.md) (point 4 replaced here), [ADR-0021](0021-a-hub-serves-the-dashboard.md)

## Context

Three buttons of the dashboard wrote files into the user's repository
through the hub:

- **"Refresh skills"** (`POST /hub/stacks/{id}/skills`): the generated agent
  skills in `.claude/skills` and the other agents' folders. Offered in the
  "stack file behind" banner too, it refreshed the skills while
  `[stack] reviewed` stayed behind, and skipped what the upgrade prompt has
  the agent check besides: the project's own skills, `CLAUDE.md` and
  `AGENTS.md` against the new devstack.
- **"Create draft"** in "Add a stack" (`POST /hub/stacks/init`): a
  `devstack.toml` from `devstack init`, before any agent saw it.
- **"Copy the main tree's"** for a worktree without a stack file
  (`POST /hub/stacks/{id}/worktrees/stack-file`, ADR-0055 point 4): the main
  working tree's file, which then still needed adapting to the branch.

Each left files nobody had read in the working tree, to be committed by
someone who did not write them. Each had a prompt beside it that does the
same and the part the button leaves out: `upgrade`, `setup`, `lane-file`.
And a button a click away is proposed again whenever a prompt seems slower.

## Decision

1. **The dashboard writes no file of a repository**, neither of the main
   working tree nor of a worktree. The hub has no route that does so for
   it. What changes a repository's files is a prompt for a coding agent
   (ADR-0046): started in the user's agent, or copied. Without an agent
   configured, the prompt's own button copies it, and the menu shows it.
2. **Git operations the user asks for are not file edits.** Making and
   removing a worktree from a branch (ADR-0047), fetching, and opening a
   folder stay: they change what git and the user's programs hold, not the
   content of a tracked file.
3. **The three are prompts now.**
   - The skills: the new prompt kind `skills` (a stack) has the agent run
     `devstack skills install` and hold the project's own skills,
     `CLAUDE.md` and `AGENTS.md` against them. The skills banner offers it
     while the stack file is up to date with this devstack; while it is
     behind, the upgrade banner speaks for both (its prompt refreshes the
     skills too), and hiding it hides both until the next minor devstack.
   - A new stack: the `setup` prompt, which runs `devstack init` itself.
   - A worktree without a stack file: the `lane-file` prompt (ADR-0055
     point 5), shown as the banner's action.
4. **The CLI and MCP stay as they are.** `devstack init`, `devstack skills
   install`, a copy by hand: the user or their agent runs them in the
   repository, which is the point of this decision.

## Alternatives considered

- **Keep the buttons, and say what they write.** Fast, but the files still
  appear unread, and the upgrade case stays half done.
- **Buttons for what is purely mechanical** (the skills after a stack file
  change). The rule then needs a judgement per button; the next button
  argues it is mechanical too. A prompt is one click more.
- **The hub commits what it writes.** It would put commits on a branch the
  user may be working on (ADR-0047 point 8 leaves commits to Git clients).

## Consequences

- Without a coding agent, a change takes copying a prompt, or the command
  the prompt names.
- `POST /hub/stacks/init`, `POST /hub/stacks/{id}/worktrees/stack-file` and
  `POST /hub/stacks/{id}/skills` are gone (`GET .../skills` stays: the
  state the banners show). A dashboard of an older release calling them
  gets `405`.

## Enforced by

`crates/devstack-hub/src/lib.rs` (the routes: none writes a repository
file), `crates/devstack-hub/src/prompts.rs` (`Prompt::Skills`),
`ui/src/components/SkillsBanner.vue`, `UpgradeBanner.vue`,
`AddStackDialog.vue`, `ui/src/views/StackLayout.vue`, the hub's tests
in `crates/devstack/tests/hub.rs` (the routes answer `405`).
