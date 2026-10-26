# ADR-0063: A lane on a node links its worktree to a copy from the local store

- **Status:** Proposed
- **Date:** 2026-10-26
- **Deciders:** project owner
- **Related:** [#277](https://github.com/nuetzliches/devstack/issues/277), [#159](https://github.com/nuetzliches/devstack/issues/159), [ADR-0044](0044-a-lanes-prepared-directories-can-be-linked-into-its-worktree.md) (point 5 replaced here for lanes on a node), [ADR-0062](0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md)

## Context

ADR-0044 links what `prepare` made into a lane's worktree, so that git hooks
there (`npx eslint` before a commit) find it. It links to the lane's copy,
which only exists when the lane runs on this machine (point 5). For a lane on
a pool node the worktree gets nothing, and the developer runs `npm ci` in it
by hand: the minutes and the second copy #159 set out to avoid. Observed
with 0.22.1, a Windows worktree, the lane on a Linux node.

The node's result would not help the worktree anyway. Hooks run on the
developer's machine, and its native packages differ from the node's; the
key of ADR-0062 tells them apart.

Linking a worktree to a store entry itself is not safe. Git 2.43 for Windows
follows a junction on `git worktree remove` and empties the directory it
points to (ADR-0044, Consequences), and Git for Windows clears a read-only
attribute before it deletes a file, so marking the entry read-only does not
stop it. One `git worktree remove` would empty an entry every lane with that
lockfile uses.

## Decision

1. **A lane on a node links its worktree too**, to a directory on this
   machine: `.devstack/lane-<name>/linked/<path>`, a copy of the local store
   entry with the worktree's key (ADR-0062 point 5). The rules of ADR-0044
   points 1 to 3 hold for it as for a local lane's link: only where the
   worktree has nothing of that name, only where git ignores the link, and
   removed when the lane stops.
2. **A missing entry is prepared here, once, in the background.** When the
   lane starts on its node, and when a file of `inputs` changes in the
   worktree, devstack takes a snapshot of the worktree, runs `prepare` in it,
   stores its `outputs` (ADR-0062 point 4) and drops the snapshot. A second
   worktree with the same inputs copies the entry; it does not prepare again.
   The lane on the node does not wait for any of this.
3. **No worktree links to the store.** A `git worktree remove` that follows a
   link empties the lane's `linked/` copy, which the next start makes again
   (as ADR-0044 point 4 does for a local lane's copy).
4. **`devstack status` and the dashboard show a link's state**, for every
   lane with `link`: `linked`, `linked (store)` (this ADR), `preparing`, or
   `not linked` with why (ADR-0044's notice).
5. **A `prepare` without `outputs`** cannot use the store: the worktree of a
   lane on a node gets no link, as before, and the notice says that
   `outputs` would give it one.

## Alternatives considered

- **Link the worktree to the store entry.** No copy at all, but one
  `git worktree remove` on Windows empties an entry shared by every lane with
  that lockfile (Context).
- **Mark the entry read-only, or deny deletion by an ACL.** Git for Windows
  clears the read-only attribute. A deny ACE on every file of a large
  `node_modules` costs a pass over all its files, is Windows-only, and leaves
  devstack owning permissions on the developer's disk.
- **Bring the node's result back.** Wrong platform for the hooks, and a
  download of tens of thousands of files.
- **Prepare in the worktree itself.** The second install #159 is about, run
  where git may remove the directory at any moment (ADR-0024).

## Consequences

- The developer's machine runs `prepare` for a lane that runs elsewhere:
  once per lockfile, in the background, with its CPU and disk. A stack that
  does not want it leaves out `link`.
- Each worktree with a lane on a node holds one more copy of its `outputs`,
  cheap where the filesystem clones (ADR-0062 point 5), a plain copy
  elsewhere, in the background.
- A hook that runs before the background `prepare` finished still fails; the
  status says `preparing`.

## Enforced by

Where it will live:

- `crates/devstack-daemon/src/supervisor/copy.rs` (`links_prepared`,
  `link_prepared`, `unlink_prepared` for lanes on a node), the store of
  ADR-0062 (`store.rs`), `crates/devstack-daemon/src/remote.rs` (the
  background `prepare` when a lane starts on a node, and on a change of
  `inputs`).
- `crates/devstack-daemon/src/api.rs` and `ui/` (the link's state),
  `crates/devstack` (`status`).
- `docs/stack-file.md` (`prepare.link`).

## What this ADR does not say

- That the lane on the node uses this machine's store. It uses its node's.
- That the main working tree gets links. It has its own directories.
