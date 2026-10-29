# ADR-0077: A set's skills are grouped by their frontmatter and switched one by one

- **Status:** Accepted (2026-10-29)
- **Date:** 2026-10-29
- **Deciders:** project owner
- **Related:** [ADR-0074](0074-a-pool-hands-out-skill-sets-the-hub-installs-them-for-the-agents.md) (skill sets, the agent table), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (the path menu), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

With the first set in use (a team's skills from one repository), three
things were missing on the Skills page:
- a way to take part of a set: a member wants one skill and not another;
- an order for a set of more than a handful of skills;
- when a skill reached this machine, as opposed to when the pool fetched it.

The page listed the agents that get skills, and the path menu's dialog
listed the programs "Open with" starts, Claude Code among them: two lists
with no link between them.

## Decision

The owner approved the review card in the conversation on **2026-10-29**,
all five points. After trying it the same day, the owner took point 5 back
(see below): it was built and removed again before the merge.

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | Who gets a set is still decided by the set alone (source, path, `to`); within a set, `metadata.group` of a skill's frontmatter groups it, for display and switching only. | A `skills.toml` of its own in the source; subfolders as groups. | Groups go; the skills stay. |
| contract | A set's manifest carries per skill `group` (and `tags` from `metadata.tags`), additively; an older coordinator leaves them out, the skills then have no group. | Groups derived in the hub (every hub would read the archives). | Fields in the pool protocol; additive. |
| concept | A member switches single skills and groups of an accepted set on or off; on accepting everything is on, a skill the set gains later too; a skill switched off goes from the agents' folders (unless changed there). | Whole sets only; new skills only after the user's word. | The switches live in the hub; without them the whole set applies again. |
| contract | `GET /hub/skills` carries per skill `enabled`, `group`, `tags`, `updated` (when this hub wrote it) and the agents that hold it; `PUT /hub/skills/sets/{pool}/{set}/skills/{name} {enabled}` and the same for a group. | Only the coordinator's fetch time. | Hub API, additive. |
| term (taken back) | "Agent" is one term: an entry of the agent table, with a skill folder and/or a way to open it. One dialog, "Programs and agents", shows both: in the menu (per browser) and skills (per machine); the Skills page links to it. | Two lists with terms of their own; moving the menu's choice into the hub. | Two dialogs again; UI only. |

What was built:

1. **Frontmatter.** `metadata:` with `group:` and `tags:` (a list in
   brackets, comma-separated, or one `- item` per line), as the Agent Skills
   format allows any keys there. A group is at most 48 characters, a tag at
   most 32, at most 16 tags. Other `metadata` keys are left alone.
2. **The hub** keeps per accepted set the skills switched off (`off`);
   accepting a set again switches all on. Writing a skill records when.
   Switching a group switches each skill of it that the set has now; the
   skills without a group are the group `-` in the route.
3. **The Skills page.**
   - Above: the agents found here, each with its skill folder and a switch.
   - Then the sets with accept, updates at once or wait, take the updates, remove.
   - Then every skill as a row, by group: a switch, the name (opens its detail: description, set and source, version, when written here, in which agents, a conflict), description, tags, state and "updated … ago".
   - Above the rows: search (name, description, tags), the set, and chips for the state (with counts) and the group.
   - A group switches all its skills at once.

**Point 5 taken back.** One dialog, "Programs and agents", showed the
path menu's programs and the agents' skills switches side by side: an
agent in the menu with a skill folder had a switch beside it, the agents
the menu cannot open a section of their own, and Orca and the editors
Cursor and Windsurf were tied to their agents of the table. Tried, it
mixed two choices that are kept in different places (the menu per
browser, the skills per machine) and raised more questions than it
answered (why Orca has no switch, whether VS Code is Copilot). The menu's
dialog is as before; the Skills page chooses the agents itself.

## Alternatives considered

- **A configuration file in the source** (`skills.toml`): groups, defaults
  and order in one place, but a second file to keep in step with the
  folders, which only devstack reads.
- **Subfolders as groups**: no file, but a layout other tools of the format
  do not expect, and a folder level that means something else for a set's
  path.
- **New skills off until switched on**: safer against a source adding
  something, but a set accepted "for all updates" would then not give its
  new skills; the set's acceptance already says the source is trusted.

## Consequences

- A member can take a set in part; its owner still decides who gets the
  set at all.

## Enforced by

`crates/devstack-node/src/skills.rs` (`Front`, `frontmatter`, `SkillEntry`),
`crates/devstack-hub/src/poolskills.rs` (`Decision::off`, `set_skill`,
`set_group`, `Installed::updated`, the view), `ui/src/views/SkillsView.vue`,
`ui/src/lib/skills.ts`.
