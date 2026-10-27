# ADR-0055: A lane whose branch has no stack file runs with the main tree's, says so, and can be given one

- **Status:** Accepted (2026-10-23); point 4 replaced by [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md) (the dashboard copies no file: point 5's prompt does it)
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (lanes are worktrees), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (what a lane on a node is synced, `[lanes] sync`), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (prompts for an agent), [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) (the dashboard makes a lane from a branch), [ADR-0050](0050-what-no-request-reports-reaches-the-dashboard-as-a-notice.md)

## Context

A repository takes devstack up on one branch: its `devstack.toml` is
committed there, or not committed at all. Every branch made before that, and
every worktree of a branch that does not have the commit yet, has no stack
file. Since ADR-0047 such a branch is one click away in the dashboard's
branch picker, and its lane behaves in two ways the user cannot see:

- **On this machine** the lane runs with the main working tree's stack file
  (`stack_file_of` in the daemon), silently. Where the branch differs (a
  service that is not there yet, another port), it fails in ways that point
  nowhere near the cause. The fallback looks for the main file's *name* at
  the worktree's root, so a stack file below the root (`-f config/devstack.toml`)
  is never found in a lane.
- **On a node** it fails at once. The node reads the stack file from the
  synced worktree (`lanerun.rs`), the worktree has none, and the start ends
  with `the stack file 'devstack.toml': …` after a lease and a sync.

Neither the branch picker nor the lane's page says that the branch has no
stack file. Giving it one means leaving the dashboard: copy the file over in
a terminal or an editor, or run `devstack init` there.

## Decision

1. **A lane's stack file is the main tree's path in its own worktree.** The
   path is the main stack file's, relative to the repository
   (`devstack.toml`, or `config/devstack.toml`). A worktree that has a file
   there runs with it; one that does not runs with the main working tree's.
2. **On a node as on this machine.** A worktree without its own stack file
   is synced with the main tree's at that path, as a `[lanes] sync` file
   missing in the worktree comes from the main tree (ADR-0029). The node
   reads what this machine would read. The main tree's file stays synced
   while the worktree has none of its own.
3. **It is shown.**
   - A lane says `main_stack_file: true` in `GET /v1/lanes`.
   - `GET /hub/stacks/{id}/branches` says `stack_file` for each branch: for
     one with a worktree, whether the worktree has the file (it may be
     uncommitted); for any other, whether the branch's last commit has it
     (one `git cat-file --batch-check` for all of them).
   - The branch picker marks a branch without one before a lane is made on
     it. The lane's page shows a banner that says the lane runs with the
     main working tree's stack file and offers point 4 and point 5.
4. **The dashboard copies the main tree's file into the worktree.**
   `POST /hub/stacks/{id}/worktrees/stack-file {path}` writes the main
   working tree's stack file, as it is on disk, to the same relative path in
   a linked worktree of the stack.
   - It never writes over a file. It never writes into the main working
     tree.
   - The file is not committed: committing is a Git client's (ADR-0047
     point 8).
   - A lane that runs keeps the stack it was started with, which is the
     same file, until its next start.
5. **An agent can adapt it to the branch.** The prompt `lane-file`
   (`GET /hub/prompt?kind=lane-file&stack=&path=`) asks a coding agent to do
   the following, started in the worktree:
   - take the main tree's file over;
   - change what differs on this branch;
   - check it with `devstack check`;
   - start the lane.

   Where the main tree's file does not fit at all, it may run `devstack init`.
6. **The main working tree is not touched** (ADR-0047 point 5).

## Alternatives considered

- **Refuse a lane without its own stack file**, here as on a node. Honest,
  but it blocks the common case: a branch older than devstack in the
  repository, which runs fine with the main tree's file.
- **Write the file whenever the dashboard makes a worktree.** Unasked, it
  puts an untracked file in every new worktree, also where the branch would
  run unchanged with the main tree's, and it leaves worktrees made with git
  or an IDE as they are.
- **Commit it to the branch.** Commits are a Git client's (ADR-0047 point 8),
  and a commit made in the background is one the user did not review.
- **A draft from `devstack init` in the worktree.** It is the
  answer for a repository without any stack file. Here the main tree has
  one, which fits the branch better than a draft with TODOs. A draft that
  does not load yet would also stop the lane, which ran with the main
  tree's file until then. It stays available to the agent of point 5.
- **A link to the main tree's file.** On Windows a symbolic link needs a
  privilege or developer mode. An edit for the branch would also change the
  main tree's file.

## Consequences

- The hub writes into a linked worktree: one file, at the stack file's path,
  never over another. ADR-0047's "devstack makes a worktree" grows by that
  file.
- The branch list costs one more git process (`cat-file --batch-check`) per
  listing, whatever the number of branches.
- A lane on a node that runs with the main tree's file gets an edit of it
  synced. As on this machine, the edit applies when the lane next starts.
- A lane that runs while the file is copied in keeps the stack it started
  with. The banner goes; a later edit of the worktree's file applies at the
  lane's next start.
- The fallback now finds a stack file below the repository's root in a lane
  too; one that was found only at the root before is no longer found there.

## Enforced by

- `crates/devstack-daemon/src/lanes.rs`: `stack_file_of`, `LaneView`
  (`main_stack_file`), and what a lane on a node is synced (`with_spec`,
  carried lanes).
- `crates/devstack-daemon/src/remote.rs`: `files`, a literal path is looked
  up without a walk.
- `crates/devstack-hub/src/branches.rs`: `stack_file` per branch, the copy;
  the route in `crates/devstack-hub/src/lib.rs`; the prompt in
  `crates/devstack-hub/src/prompts.rs`.
- `ui/src/components/BranchPicker.vue`, `ui/src/views/StackLayout.vue`,
  `ui/src/stores/stack.ts`.
- `docs/dashboard-api.md`, `docs/stack-file.md` (lanes).

## What this ADR does not say

- Nothing about a main working tree without a stack file (switched by a Git
  client to a branch without one): the stack does not load, as before.
- Nothing about other files a stack file names (Compose files, a lane
  image's Dockerfile) that the branch lacks: the lane fails on them as it
  would with any stack file, and the agent of point 5 is the way to adapt
  them.
