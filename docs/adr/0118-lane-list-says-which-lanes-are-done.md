# ADR-0118: `lane list` says which lanes are done, and devstack removes none of them

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-23
- **Deciders:** project owner
- **Related:** issue #606, [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a worktree is a lane), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) (`lane prune`), [ADR-0088](0088-a-lane-shows-how-far-its-branch-is-ahead-and-behind.md) (ahead and behind from the local refs), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

Agents leave a worktree behind per task. After a busy day a repository had
eleven, each a stopped lane, all of them on `main` already. `devstack lane
prune` said "nothing to prune: every lane's data belongs to a worktree",
which was true, and `lane list` showed branch and ports, but nothing said
which lanes were done. Finding out took long and went wrong: `git branch
--merged` and `git cherry` miss squash merges, a detached worktree has no
branch to ask about, and on Windows two `vite preview` processes of an ended
session held a native module open, so `git worktree remove` stopped half way
and left an unregistered folder.

## Decision

The owner approved the points in the conversation on **2026-11-23**, all
five:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | `devstack lane list` gets a column `DONE` with `merged`, `squashed <sha>`, `open`, `dirty` or `busy`; the MCP tool `lanes` says the same per lane (`done`). | A command of its own (`lane prune --merged`) one has to know. | A column and a field scripts and agents read. |
| 2 | concept | A lane is done when its HEAD is on the base (`merged`) or every file it changed since the merge base has the same content in a later commit of the base (`squashed`, with that commit), and it is neither `dirty` nor `busy`; a detached worktree is judged by its HEAD the same way. | `git branch --merged` alone, which misses squash merges. | Low: a rule in the CLI. |
| 3 | concept | The base is the remote's default branch as a remote ref (`origin/HEAD`, else `origin/main`, else a local `main` or `master`), compared without a fetch, as last fetched. | The local `main` (often behind), or a new key in the stack file. | Low; a key can come later. |
| 4 | trust | devstack removes nothing: a done lane gets a hint with the commands in their order (`git worktree remove`, `git branch -d`, or `-D` when squashed, `devstack lane prune`; a running lane first `devstack lane stop`). | Removing on its own, which could delete unsaved work. | None. |
| 5 | concept | `busy`: the lane runs, or a process has its working directory, its program or an argument of its command line in the worktree, read with the new dependency `sysinfo` (MIT), on Windows, Linux and macOS. | Only the processes devstack started, which leaves the `vite preview` trap of the issue. | One dependency less. |

Decided while building, not on the card: the check runs in the CLI, not in
the daemon, which gives only the lane's path, branch and whether it runs;
the dashboard does not show it (a follow-up if wanted); the processes of the
CLI itself and of those that started it (the asking shell or agent) do not
count; `dirty` and `busy` are said only of a lane on the base (an open lane
is `open`, whatever else); the squash commit named is the one with the last
of the lane's file contents.

As built:

1. **The check.** `crates/devstack/src/lanedone.rs`: `base` reads
   `refs/remotes/origin/HEAD` and falls back as point 3 says; `on_base` asks
   `merge-base --is-ancestor`, else takes `git diff --raw` from the merge base
   to HEAD (the blob each changed path ends with, or deleted) and walks
   `git log --reverse --raw` of the base since the merge base, one git call,
   until every path met its content; `dirty` is `git status --porcelain`;
   `Procs` reads the process table once per listing.
2. **The output.** `lane list` prints the column and, under the table, one
   line per lane on the base: done with the commands, or what keeps it.
   `lane prune` with nothing to prune points to `lane list`. The MCP's
   `lanes` adds `done: …` and the same line.
3. **Tests.** `lanedone.rs`: the base, merged, squashed with later work on
   the same file and a deletion, a partial squash and other content left
   open, dirty, busy by a run and by a process, folder matching by whole
   components, the hint's order.

## Consequences

- `lane list` and the MCP's `lanes` run git a few times per lane and read the
  process table once; on a repository with many worktrees that is a second
  or so.
- A lane is `open` until its branch's work is on the base as last fetched: a
  merge on the server shows after the next fetch.
- `busy` names the first process found; a process whose command line holds a
  relative path into the worktree, and no working directory there, is not
  seen.

## Enforced by

- `crates/devstack/src/lanedone.rs` and its tests; `crates/devstack/src/client.rs`
  (`lane_list`), `crates/devstack/src/mcp.rs` (`lanes`).
- `docs/stack-file.md` (`devstack lane list`), `skills/devstack/SKILL.md`.
