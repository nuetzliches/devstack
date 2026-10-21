# ADR-0047: The dashboard makes a lane from a branch; the main tree's checkout stays the user's

- **Status:** Accepted (2026-10-21)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [ADR-0019](0019-control-api-security.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (point 1 and its second amendment, which this replaces in part), [ADR-0040](0040-a-shared-lane-can-be-watched-like-ones-own.md), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md)

## Context

A lane is a worktree (ADR-0024). Running another branch therefore starts with
`git worktree add <dir> <branch>`, typed in a terminal, or done by an IDE or an
agent tool. The dashboard shows the lanes that exist and starts them; it cannot
make one. Someone who works from the dashboard (a reviewer who wants to run a
colleague's branch, the owner checking a pull request) leaves it for a terminal
for exactly one command, and must choose a folder for it.

The header's lane picker no longer earns its place: the sidebar lists a
stack's lanes and switches between them. The place is free for the step that
comes before a lane exists.

ADR-0024 point 1 says devstack never creates or removes a worktree, and its
second amendment (2026-10-15) took `lane add` and `lane remove` out again:
they wrapped `git worktree` "with a path convention of their own, a second way
to the same result that agent tools and IDEs do not know". That reason holds
for the CLI and the MCP server, whose users have git at hand. It does not hold
for the dashboard, whose users by definition do not, and it is met if the
folder follows the worktrees the repository already has.

Switching the branch of the main working tree is what users ask for first, and
what does the most harm: the main tree's services, candidates and workers
(ADR-0008) run from it, uncommitted changes are in it, and an IDE has it open.
devstack already has the switch without a checkout: a lane takes the main
ports (`promote --lane`, ADR-0024 point 5, **Take main ports** in the header).

## Decision

1. **The header lists branches, not lanes.** For one's own stack, the picker
   lists the repository's local branches and its remote-tracking ones
   (`git for-each-ref refs/heads refs/remotes`), each marked with the worktree
   that has it checked out (`git worktree list --porcelain`) and that lane's
   state. Per branch: **Show** (its lane, if a worktree has it), **New lane**
   (one without), **New branch from here** (a name, then a lane on it), and for
   a lane that is stopped **Remove lane**. A lane shared with the user
   (ADR-0040) shows no picker: its worktree is on someone else's machine.
2. **devstack makes a worktree, from the dashboard only.** `New lane` runs
   `git worktree add <dir> <branch>` (a remote-tracking branch: `--track -b
   <local> <dir> <remote>/<branch>`); `New branch` runs `git worktree add -b
   <new> <dir> <base>`. The daemon finds the worktree as it finds any other
   (ADR-0024 point 1), and the page shows the lane, not started. The CLI and
   the MCP server get no such command: their users run git.
3. **The folder follows the repository's worktrees.** `<dir>` is in the
   directory the repository's most recently added linked worktree is in, so a
   repository whose worktrees an IDE or Orca places keeps them in one place.
   A repository without one gets `<parent>/<repo>.worktrees/`, beside it, never
   inside it (a file watcher, a search and a build would see it twice). The
   folder's name is the branch with `/` as `-` (`feat/login` → `feat-login`),
   which is the lane's name; a name taken gets `-2`, `-3`.
4. **Removing is git's, with git's checks.** `Remove lane` is offered only for
   a stopped lane and runs `git worktree remove <dir>` without `--force`: a
   worktree with changes is refused with git's message, and the page says to
   commit, stash or remove it elsewhere. The branch stays.
5. **The main tree's checkout is never changed.** No checkout, switch, reset or
   pull in the main working tree. To run a branch on the main ports, the page
   offers **New lane**, then **Take main ports**. That moves the fixed ports
   of `[services.*]` only: for a stack of Compose containers only, the slot
   has nothing to move and is refused (#184), so the page offers no **Take
   main ports** there and the switch is a lane on its own ports.
6. **The hub runs git, the page names.** `GET /hub/stacks/{id}/branches`,
   `POST /hub/stacks/{id}/worktrees {branch, new?, base?}`,
   `POST /hub/stacks/{id}/worktrees/remove {path}` (cookie and Origin,
   ADR-0019). The hub runs them for a registered stack also while it is
   stopped. A branch must be one the hub just listed; a new one must pass
   `git check-ref-format --branch`; refs go after `--`; the folder is the
   hub's, never the page's; a path to remove must be a linked worktree of that
   repository (as `POST /hub/open` checks, ADR-0046). git runs with
   `GIT_TERMINAL_PROMPT=0` and `GCM_INTERACTIVE=never`, no window, and a
   timeout.
7. **No fetch of its own accord.** Remote-tracking branches are listed as the
   last fetch left them. A **Fetch** button runs `git fetch --prune` for the
   repository's remotes; a remote that asks for credentials fails with git's
   message instead of showing a prompt from a background process.
8. **Everything else is a Git client's.** Commits, staging, push, merge,
   rebase and conflicts are not devstack's; the path's menu opens the tree in
   one (GitHub Desktop, ADR-0046).

## Alternatives considered

- **Lean: only a Git client in the path's menu.** Cheap, and done (ADR-0046).
  It does not make a lane: GitHub Desktop switches the branch of the tree it
  opens, which is the main tree's checkout this ADR keeps untouched.
- **Switch the main tree's branch from the dashboard, with a confirmation.**
  What a Git client does. It restarts the main tree's services on other
  sources, moves its workers' code under them, and fails or carries changes
  along with uncommitted work. A lane on the main ports gives the same result,
  and **Take main ports** with `main` undoes it.
- **Bring back `lane add` / `lane remove` in the CLI as well.** The reason for
  removing them still holds there: agents and IDEs make worktrees with git or
  their own tools, and a second command with its own rules is what they do not
  know.
- **The daemon runs git.** It owns the repository's lanes, but runs only while
  the stack does; making a lane for a stopped stack, then starting it, is the
  common case.
- **A folder configured per stack (`[lanes] dir`).** A key in the stack file
  for a choice that is the user's, not the stack's, and that differs per
  machine. Following the existing worktrees needs no setting; one can be added
  later in the hub's own settings if that is not enough.

## Consequences

- ADR-0024 point 1's "devstack never creates or removes one" holds for the CLI,
  the MCP server and the daemon, no longer for the hub. Its second amendment's
  reason is answered by point 3, not void: a repository with no worktrees yet
  gets devstack's folder, `<repo>.worktrees/`.
- The hub needs `git` on `PATH`, as the daemon does for its lanes.
- A remote branch that was never fetched is not listed until **Fetch**; a
  remote that needs an interactive login (a GCM browser prompt) cannot be
  fetched from the dashboard.
- Removing goes through git's checks only: an untracked build output in the
  worktree makes `git worktree remove` refuse; the page cannot force it.
- Another state that can go stale: a branch deleted or checked out elsewhere
  between listing and clicking. The hub answers what git says.
- The header's lane picker goes; switching lanes is the sidebar's.
- Until a Compose container's published port can move with the main slot
  (#184, an ADR of its own), a stack of containers only has no "switch":
  its other branch runs on the lane's ports.

## Enforced by

Where it will live:

- `crates/devstack-hub/src/branches.rs` (new: listing, the folder, `git
  worktree add`/`remove`, fetch), routes in `crates/devstack-hub/src/lib.rs`.
- `crates/devstack-daemon/src/lanes.rs` (`worktree_of`, the check a path to
  remove is a linked worktree).
- `ui/src/components/StackHeader.vue` (the branch picker replaces the lane
  picker), `ui/src/stores/stack.ts`.
- `docs/dashboard-api.md` (the three routes), `docs/plan.md` §3.5.

## What this ADR does not say

- Nothing about pull requests (checking one out by number, `gh`): a branch
  fetched is listed; a PR's head is a later question.
- Nothing about deleting branches, local or remote.
- Nothing about lanes on a node (ADR-0025): a lane made here is started where
  the user chooses, as any other.
