# ADR-0074: A pool hands out skill sets; each member's hub installs them into the coding agents' user skill folders

- **Status:** Accepted (2026-10-29), extended by [ADR-0079](0079-skills-local-to-a-working-tree.md)
- **Date:** 2026-10-29
- **Deciders:** project owner
- **Related:** [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (the pool's MCP upstreams, the model for acceptance and precedence), [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md) (nothing goes into a repository), [ADR-0073](0073-a-pool-tells-its-members-what-changed-over-one-stream.md) (how a change reaches the members), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) ("Open with"), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A team keeps agent skills (folders with a `SKILL.md`, the Agent Skills
format) that make its agents work its way: generic ones for every
repository, and ones for a project. Checked into the repositories they are
handed to whoever gets the repository, and kept once per harness folder
(`.claude/skills`, `.agents/skills`, ...), drifting apart. A plugin
marketplace reaches one harness only and updates by version number. The
team wants one source, outside the repositories, available to every agent
on every member's machine without per-harness setup, and updates there
within seconds of a push.

The agents read skills from a folder in the repository or from a user
folder of their own (`~/.claude/skills`, `~/.agents/skills`,
`~/.config/opencode/skills`, ...); the open-source `skills` CLI of
vercel-labs keeps the table of them for some seventy agents.

## Decision

The owner approved the review card in the conversation on **2026-10-29**,
all seven points:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | The coordinator of a pool hands out skill sets; each member's hub copies them into the user skill folders of the agents it finds on the machine. Not into a repository, not through MCP. | Skills through `devstack mcp`; `npx skills` per machine and agent; a Claude marketplace (one agent, version bumps). | The hub removes what its lock says it wrote; the endpoints go. |
| contract | The pool's API: `GET /v1/skills` (the sets given to the member, `sha256` per skill) and `GET /v1/skills/{set}/{skill}.tar.gz`; the topic `skills` on the stream (ADR-0073) tells a change. A coordinator of an earlier release answers `404`, read as none. | The hub fetches from Git itself (credentials on every machine, no central revocation). | Pool protocol with users outside the maintainers; additive. |
| trust | The coordinator fetches the sources with its own access: `github` (the repository's archive over the API, a read token from its environment) and a folder on the coordinator. Which member gets which set is the pool's allocation; every download is in the audit log. If wrong: a member gets the knowledge of a project it does not work on. | Access by each member's GitHub rights; `git` in the coordinator's image. | More source kinds are additive; dropping the allocation gives everyone everything. |
| trust | A set is used only once the member accepted it in the dashboard; then its updates come at once or after the user's word, as the member chooses per set. If wrong: whoever can push to a source puts instructions and scripts in front of every agent without anyone clicking. | Sets active at once; automatic updates chosen per agent. | The choice is kept by the hub; making the word compulsory later only changes the default. |
| concept | The user switches each agent found on or off. The hub writes only folders of its lock; a foreign or hand-changed skill of the same name stays and is shown as a conflict. | The pool overwrites; every agent found, unasked. | Changing the precedence later overwrites users' skills. |
| trust | `POST /v1/skills/refresh` takes a request without a client certificate, signed with a secret from the environment (HMAC); it only has the sources fetched again and takes no content. A push through a webhook relay starts the distribution at once. If wrong: whoever has the secret can only cause fetches; a fetch runs one at a time. | An admin or a timer fetches (polling). | Remove the endpoint; admins fetch by hand. |
| concept | One agent table, taken from `vercel-labs/skills` (MIT) as data and kept in step by a script: skill folders and how each agent is told installed; "Open with" takes its agents from it too, how one is started staying its own field. | A list of our own; two lists that drift apart. | Keeping two lists again costs only upkeep. |

What was built:

1. **Sets** (`skills.json` in the coordinator's directory, changed by
   admins with `devstack pool skills add|remove` or in the pool area,
   each change audited): a name, a source, and `to` (`*`, member ids,
   tags). A GitHub source is `owner/name`, a branch, tag or commit, and
   the folder of the skills in it; a folder source is below
   `skill-sources/` in the coordinator's directory (no `..`), so an admin
   cannot hand out the coordinator's keys.
2. **A fetch** reads the source, takes each folder with a `SKILL.md` whose
   frontmatter names the folder (1 to 64 of `a-z0-9-`) and has a
   description, packs it as a `.tar.gz` (the same bytes for the same files)
   and writes the set's manifest: skills with SHA-256 and size, the commit,
   the folders left out and why. Limits as the `skills` CLI has them: 10 MiB
   an archive, 25 MiB and 1000 files unpacked; a repository's archive 200
   MiB. A new fetch is written beside the last and swapped in; one that
   fails keeps the last, with the error in the manifest. Fetches run one at
   a time: when the coordinator starts, when an admin saves or asks, and on
   a signed refresh.
3. **The signed refresh** is signed as Hookaido signs a delivery:
   `X-Hookaido-Signature` = `hex(HMAC-SHA256(secret, "<METHOD>\n<PATH>\n<TS>\n<sha256(body)>"))`,
   `X-Hookaido-Timestamp` within five minutes. Without
   `DEVSTACK_SKILLS_REFRESH_SECRET` the route answers `404`. The read token
   is `DEVSTACK_SKILLS_GITHUB_TOKEN`; without it only public repositories.
4. **The stream** tells each member the version of its own sets: another
   member's set changing is not told.
5. **The hub** reads a pool's sets when the stream says `skills` changed,
   keeps archives by SHA-256 (`skill-store/`, checked against the
   manifest), and installs into the user folders of the agents switched on;
   agents sharing a folder (`~/.agents/skills`) get it once. A set is
   accepted with its source; a source that changes is offered again. With
   updates waiting, the skills stay at the versions the user agreed to
   until "Take the updates". The lock (`skills.json` in the hub's state)
   records folder, skill, set, SHA-256 and a hash of the files written.
   A skill no accepted set names any more is removed, unless it was
   changed there: then it stays, the user's now. Two accepted sets with a
   skill of one name: neither is installed. Writes go through a folder
   beside the target, swapped in.
6. **The dashboard**: a page *Skills* (agents with their folder and switch,
   the sets with accept, decline, updates at once or not, take the updates,
   the conflicts), its state on `/hub/events` (`skills`); the pool area
   lists the sets for admins with their last fetch. The CLI: `devstack pool
   skills [list|add|remove|refresh|status|sync]`.
7. **The agent table** is `crates/devstack-skills/agents.toml`, written by
   `scripts/sync-agents.py` from `src/agents.ts` of vercel-labs/skills
   (69 agents; ones found another way than by fixed paths are left out). A
   path is `~/...` or `${VAR:-default}/...`. An agent is found when one of
   its `detect` paths exists. `[agent.open]` (devstack's own, kept by the
   script) names the URL schemes "Open with" starts it by: Claude Code and
   Claude Desktop now come from there; the editors stay a list of their own.

## Alternatives considered

- **Skills through `devstack mcp`** (a tool per skill). Works in every
  harness that has it, per stack too, but no harness triggers a skill from
  a tool as it does a native one, and scripts and references would not be
  on disk.
- **Per-repository installation** (a skill folder in the repository, kept
  out of Git). Scopes a project's skills to its repository, but writes into
  the repository (ADR-0071) and needs a step per clone.
- **The hub fetches from Git** with the member's credentials. Access by
  GitHub's rights per member, but no revocation through the pool, and Git
  credentials on every machine.
- **Symlinks into the agents' folders** (as the `skills` CLI prefers). A
  directory symlink needs Developer Mode on Windows.

## Consequences

- Every skill a member accepts reaches every agent switched on, in every
  repository: a project's skills are visible in sessions for other
  projects. The allocation keeps them to the project's members; a skill's
  description says which repository it is for. Since ADR-0079 a skill
  bound to repositories (`metadata.repos`) goes into their working trees
  only, and a set's folders can go to different members.
- A member removed from the pool loses its sets: its stream ends and its
  hub removes their skills (but ones it changed).
- An agent picks up a skill's new content at its next use; a new or
  removed skill, or a changed description, may need a new session,
  depending on the agent.
- Not built: sessions told of an update (agent-specific channels).

## Enforced by

`crates/devstack-node/src/skills.rs` (sets, sources, packing, the
signature), `crates/devstack-node/src/coordinator.rs` (`skills_list`,
`skill_archive`, `skills_refresh_signed`, `admin_skill*`, `fetch_skills`,
the per-member version in `events`), `crates/devstack-node/src/member.rs`,
`crates/devstack-hub/src/poolskills.rs`, `crates/devstack-hub/src/poolfeed.rs`,
`crates/devstack-hub/src/open.rs` (`links`), `crates/devstack-skills/src/agents.rs`,
`crates/devstack-skills/agents.toml`, `scripts/sync-agents.py`,
`crates/devstack/src/poolcmd.rs` (`skills`), `ui/src/views/SkillsView.vue`,
`ui/src/components/PoolSkills.vue`, `crates/devstack-node/tests/skills.rs`.
