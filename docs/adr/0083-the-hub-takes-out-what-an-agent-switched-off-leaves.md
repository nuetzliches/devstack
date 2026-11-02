# ADR-0083: The hub takes out what an agent switched off leaves: devstack's generated skills, and the folders a removed skill empties

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-02
- **Deciders:** project owner
- **Related:** [ADR-0074](0074-a-pool-hands-out-skill-sets-the-hub-installs-them-for-the-agents.md) (the agents switched on, the pool's skills), [ADR-0079](0079-skills-local-to-a-working-tree.md) (devstack's own skills in a working tree; point 5 amended here), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A member switched three of four agents off on the Skills page. The pool's
skills went from their folders, `.agents/skills` among them, as the lock
says. devstack's generated skills (`devstack`, `devstack-config`) stayed:
the hub writes them into the folders of the agents switched on and, by
ADR-0079 point 5, refreshes a generated copy it finds in any other agent's
folder. Nothing removed one, and the refresh kept it current, against the
user's choice. And removing a skill took its own folder only: an emptied
`.agents/skills`, and `.agents`, stayed behind, in the working trees and
in the home.

## Decision

The owner approved the points in the conversation on **2026-11-02**:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The hub removes a generated devstack skill (unchanged, untracked) from a folder in a working tree that no agent switched on reads and `[agents] skills` does not name. | Refreshing such a copy everywhere and never removing it (ADR-0079 point 5). | Low: switched on again, the agent gets the copy written anew. |
| 2 | concept | After removing a skill (a pool's or devstack's), the hub removes the folders it left empty above it, at most up to the working tree's root or the home. | Empty `.agents/skills` and `.agents` left behind. | Low: a folder is made again when a skill is written. |

As built:

1. **Which folders.** The working-tree folders (`project` in the agent
   table) of the agents the user switched off, less those an agent
   switched on reads (`.agents/skills` is shared by several). An agent
   never switched on or off keeps the old rule: a copy there is refreshed.
   A folder `[agents] skills` names stays, `.claude/skills` too when the
   key names none: `devstack skills install` writes there.
2. **Which copies.** A skill folder with the generated marker that holds
   only `SKILL.md` and its `.gitignore`; one git tracks, or with a file the
   project put beside it, stays. Whether the text was edited is not held
   against the templates: the hub overwrote such a copy before as well.
3. **Pruning.** `devstack_skills::prune_empty` removes a folder and its
   parents while each is empty, below the stop, which stays. For a pool
   skill the stop is the root of the working tree its folder is in, else
   the home when the folder is an agent's user folder, else nothing goes.
   For a generated skill it is the folder of the stack file.
4. **Tests.** The hub's tests no longer reach this machine's registry of
   stacks: the stacks `local()` writes into are a field, the registry in
   the hub, a list of its own in a test.

## Consequences

- Switching an agent off clears its folders of everything devstack wrote,
  in the home and in every working tree, and leaves no empty folder.
- A user folder emptied of pool skills goes too (`~/.claude/skills` when
  no skill of the user's own is left); the agent reads none either way.
- An empty folder above a removed skill goes even when the user made it:
  the hub cannot tell who made an empty folder.
