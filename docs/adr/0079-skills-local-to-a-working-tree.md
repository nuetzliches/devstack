# ADR-0079: Skills local to a working tree: a pool's project skills bound to their repository, devstack's own untracked

- **Status:** Accepted (2026-10-30)
- **Date:** 2026-10-30
- **Deciders:** project owner
- **Related:** [ADR-0074](0074-a-pool-hands-out-skill-sets-the-hub-installs-them-for-the-agents.md) (skill sets, the agent table; its "per-repository installation" alternative taken up here), [ADR-0077](0077-a-sets-skills-are-grouped-and-switched-one-by-one.md) (`metadata` of a skill), [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md) (amended here), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (the working trees the hub follows), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

ADR-0074 installs a pool's skill sets into the agents' user skill folders.
That is right for generic skills; a set that also carries a project's
skills puts them into every session on the machine, in every repository.
Project skills tend to have generic triggers ("new page", "add a button")
and very specific content (one codebase's libraries and paths): an agent in
another repository picks one and scaffolds against libraries that are not
there, and the descriptions cost context in every session. The stopgap was
a description that says "only in `<owner>/<name>`" and a first step that
checks `origin`.

A team that keeps generic and project skills in one source had to register
a set per folder to give the project folder only to the people on that
project.

devstack's own generated skills (`devstack skills install`) were committed
to each repository, although what they say is decided by the devstack
installed on each machine: every update changed them, and two teammates on
two versions wrote them back and forth.

Trying the options showed:

- A skill offered through `devstack mcp` (its description in the server's
  `instructions`, its text from a tool) is triggered by prose instead of
  the agent's skill mechanism, costs a round trip in harnesses that defer
  MCP tools, and has its scripts outside the workspace (permission prompts,
  sandboxes).
- Claude Code (2.1.312) and OpenCode find a skill in `.claude/skills` and
  `.agents/skills` of a working tree whose folder ignores itself with a
  `.gitignore` of `*`; Claude Code runs it. Codex and the Copilot CLI were
  not tried.
- devstack's file watcher reads nested `.gitignore` files, so a folder
  that ignores itself does not start a build.

## Decision

The owner approved the review card in the conversation on **2026-10-30**,
all seven points:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | Each working tree has a local, untracked layer of skills: devstack's generated skills and the pool's skills bound to its repository. The hub writes them into the skill folders in the working tree of the agents switched on (each agent's folder from the agent table; the user's switches per machine decide), for every stack and worktree it follows (ADR-0078). | MCP `instructions` with a `skill` tool; user folders with a rewritten description; `.devstack/skills` with a pointer from `AGENTS.md`. | The layer goes; devstack's skills are committed again, bound ones go into the user folders. |
| trust | Every folder devstack writes there ignores itself (`.gitignore` with `*`); the hub's lock records what it wrote. A foreign or hand-changed skill of the same name stays and is shown as a conflict; a file git tracks is never written. ADR-0071 is amended: ignored copies devstack owns are allowed, tracked files stay off limits. If wrong: the hub overwrites a repository's own skill or one the user changed. | An entry in the repository's `.gitignore` (tracked, a list of names); `.git/info/exclude` (the hub writes git's configuration). | The exception in ADR-0071 goes; the hub removes what its lock names. |
| concept | devstack's generated skills are no longer tracked: their version follows the local devstack, and the hub writes them again when devstack or the stack file changes. `devstack skills install` writes them the same way, ignore included. The upgrade prompt has the agent take tracked copies out of git once. Agents in the cloud and CI, which see tracked files only, have no devstack skills then. | Tracked copies that change with every update and flip between teammates on different versions. | They are tracked again, through the prompt. |
| contract | `metadata.repos: [acme/shop]` in a skill's frontmatter binds it, matched against the last segments of the path of the working tree's `origin`, case and host aside. A bound skill goes only into the agents' folders of the matching working trees, never into the user folders; a skill without `repos` behaves as before. An older hub ignores the field. | A binding by a flag of the set only. | A field in the pool protocol; without it the skills are everywhere again. |
| contract | A set can name several folders, each with its own `to` (and `repos` for its skills without their own); a set of one `path` and `to` stays as it is. | A set per folder (source, acceptance and updates several times). | The pool protocol and `skills.json`; a set of folders would have to be split into sets again. |
| trust | The coordinator checks which folder goes to whom for a skill's archive too, not only in the list. If wrong: a member downloads the knowledge of a project it does not work on. | Filtering the list only. | None; it is only stricter. |
| contract | `GET /hub/skills` carries per skill `repos` and the working trees it is in; the Skills page shows both. | The binding in a skill's detail only. | Hub API, additive. |

What was built:

1. **Frontmatter.** `metadata.repos`, a list as `tags` is (brackets,
   commas, or one `- item` per line): `owner/name`, or more segments
   (`project/_git/name`), at most 16; an entry with a host or other
   characters is left out.
2. **Matching.** The URL of `origin` (`git@host:acme/shop.git`,
   `https://host/acme/shop`, `ssh://git@host:22/acme/shop.git`) is cut to
   its path; an entry matches when it equals the last segments of it, case
   aside. A working tree without `origin` matches nothing. Worktrees share
   their repository's `origin`.
3. **Sets of folders.** `folders: [{path, to, repos}]` in the coordinator's
   `skills.json`, each `path` below the source's; with folders, the set's
   own `to` and `repos` are empty. A fetch reads each folder (a GitHub
   source's archive once) and records per skill `folder` and `repos` (its
   own, else its folder's). A skill name in two folders of a set is left
   out of both. `GET /v1/skills` gives a member only the skills of its
   folders, `GET /v1/skills/{set}/{skill}.tar.gz` answers `404` for the
   others. `devstack pool skills add` takes `--path` with `--to` (and
   `--repos`, `-` for none) once per folder; against a coordinator that
   drops the folders or the binding, it takes the set out again and says
   so. The pool area edits folders as rows.
4. **The hub, a pool's bound skills.** The working trees are the main
   trees and worktrees of the stacks registered here, `origin` asked of
   git once per repository. A bound skill goes into the folder of each
   agent switched on that has one in a working tree (the agent table's
   `project`, from `skillsDir` of vercel-labs/skills: `.claude/skills` for
   Claude Code, `.agents/skills` for Codex, Cursor, Copilot, OpenCode and
   most others), in every matching working tree, once per folder; its
   folder gets the `.gitignore`. Before writing, the hub asks git whether
   it tracks a file there; then it writes nothing and shows a conflict. A
   worktree that goes takes its entries out of the lock. A skill that gains
   `repos` leaves the user folders.
5. **The hub, devstack's own skills.** For the same working trees, the hub
   renders the generated skills from the stack file the tree runs with
   (its own, else the main tree's) and writes them into the folders of the
   agents switched on, next to the stack file, refreshing a generated copy
   found in another agent's folder. A copy git tracks is left alone; the
   banner offers the prompt that untracks it. Nothing is written for a tree
   whose stack file does not load.
6. **When.** A thread of the hub looks every three seconds at the
   worktrees' version (ADR-0078) and the registered stack files' times,
   and writes both kinds again when one changed, once a minute at the
   latest; switching an agent writes at once.
7. **The CLI.** `devstack skills install` writes the `.gitignore` into
   every generated skill's folder and lists the folders git still tracks,
   with the `git rm -r --cached` that takes them out (the files stay).
8. **The prompts.** The upgrade and skills prompts have the agent take
   tracked generated skills out of git, whatever version the repository
   comes from: a repository may skip releases, and the step does nothing
   without such copies.
9. **The dashboard.** The Skills page shows each agent's folder in a
   working tree beside its user folder, a bound skill's repositories as a
   pill and in its detail with the working trees it is in, and the state
   *no working tree here* for one whose repositories are no stack on this
   machine.

`[agents] skills` in the stack file still names the folders `devstack
skills install` writes; the hub goes by the user's agents instead. The key
stays as it is: no stack file changes meaning.

## Alternatives considered

- **Bound skills through `devstack mcp`** (descriptions in the server's
  `instructions`, the text from a `skill` tool, the files in the hub's
  state). Knows the repository and needs no file in the working tree, but
  is triggered by prose instead of the agent's skill mechanism, needs a
  harness that loads `instructions`, costs a round trip where MCP tools are
  deferred, and puts scripts outside the workspace.
- **User folders with a rewritten description** ("only in `acme/shop`")
  and an `origin` check. Works in every agent, but the descriptions still
  load everywhere: the context cost and the false triggers stay.
- **`.devstack/skills` with a pointer from `AGENTS.md`.** One folder of
  devstack's own, but no agent reads skills from there, and the pointer is
  a tracked file that changes with the skills.
- **A plugin per project** (Claude Code, a local marketplace enabled per
  project). One agent only, and an entry in the project's settings file.
- **An entry in the repository's `.gitignore`.** One place, but a tracked
  file to change, and a list of names that changes with the pool's sets.
- **`.git/info/exclude`.** Untracked and shared by the worktrees, but the
  hub would write git's own configuration.

## Consequences

- A project's skills reach its members (the folder's `to`) and, on their
  machines, the working trees of its repository only: twice scoped.
- A repository that is no stack registered here gets no bound skills and
  no devstack skills from the hub; `devstack skills install` still writes
  the latter.
- An agent that reads both `.claude/skills` and `.agents/skills` sees a
  bound skill twice when both are switched on, as with devstack's skills
  before.
- `git clean -fdx` removes the local layer; the hub writes it again.
- A coordinator before this release ignores `folders` and `repos`; a hub
  before it installs bound skills into the user folders, as ADR-0074 did.
- Not tried: whether Codex and the Copilot CLI find skills in a folder git
  ignores.

## Enforced by

`crates/devstack-node/src/skills.rs` (`SkillFolder`, `repo_matches`,
`folder_given_to`, `skill_given_to`, `pack`), `crates/devstack-node/src/coordinator.rs`
(`skill_archive`), `crates/devstack-hub/src/poolskills.rs` (`places`, `local`,
`follow`, `registered_trees`, `install`), `crates/devstack-hub/src/branches.rs`
(`origin`, `tracked`), `crates/devstack-skills/src/lib.rs` (`IGNORE_ALL`,
`install_into`), `crates/devstack-skills/agents.toml` (`project`),
`scripts/sync-agents.py`, `crates/devstack/src/poolcmd.rs`,
`crates/devstack/src/dispatch/skills.rs`, `crates/devstack-hub/src/prompts.rs`,
`ui/src/views/SkillsView.vue`, `ui/src/components/PoolSkills.vue`,
`ui/src/lib/skills.ts`, `crates/devstack-node/tests/skills.rs`.
