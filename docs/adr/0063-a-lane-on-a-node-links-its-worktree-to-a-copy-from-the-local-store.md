# ADR-0063: A lane on a node links its worktree to a copy from the local store

- **Status:** Accepted (2026-10-26); built, noted 2026-10-26; addendum 2026-10-27 (two copies: new inputs are prepared beside the one the worktree has; an emptied copy is prepared again)
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

*Paths as built, noted 2026-10-26.* `crates/devstack-daemon/src/supervisor/copy.rs`:
`link_node_lane` (the key from the worktree's inputs as a copy would hash
them, `run_prepare` with the copy's source synced only before a `prepare`
itself, not before a hit), `unlink_node_lane`, `link_one` (ADR-0044's rules,
shared with a local lane's `link_prepared`), `NodeLink`. The copy is
`.devstack/lane-<name>/linked/src/<path>` (a snapshot of the worktree, which
gets the source only when it has to prepare), not `linked/<path>`.
`crates/devstack-daemon/src/lanes.rs`: `follow_node_links` (every 15
seconds over the lanes on nodes; a look at the inputs while nothing
changed), `unlink_node` (on `lane stop`, the lane forgotten, and for any
lane that left its node), `LaneView::links`. Point 2's "when a file of
`inputs` changes" is that check, not the watcher. Shown in `devstack lane
list` and `status` (`crates/devstack/src/client.rs`) and in the dashboard's
lane header (`ui/src/components/StackHeader.vue`). `prepare` for a worktree
runs under the main tree's one-at-a-time lock and logs as the main tree's
service.

## Addendum 2026-10-27: two copies, the link moves when the new one is whole

Point 2 ran `prepare` for new inputs in the copy the worktree was linked to.
`npm ci` empties `node_modules` first: while it ran, the worktree's hooks
found a few packages, and when it failed (a file held open, a machine out
of memory, #305) the link stayed on what was left, with nothing in the log.

A lane on a node now has two copies, `linked/src` and `linked/next/src`, each
with its own `prepared/<service>` marks. New inputs are prepared (from the
store, or by running `prepare`) in the copy the worktree is *not* linked to;
its mark goes first, so a half-made copy is no copy of any key. Once it
succeeded the link moves there. When it fails the worktree keeps the copy
it had, made for its inputs before, and the path says `linked (stale)` with
why; the next look tries again. Inputs that go back to those of the other
copy only move the link. A failed `prepare` is logged (`prepare_failed`,
with its log). The disk a lane's copies take doubles at most.

*Amended 2026-10-27 (#311):* a copy that is linked is also checked for
being whole. With its mark, each linked directory's entries are counted
(`prepared/<service>.entries`); one that has lost more than a tenth of them
since (a reinstall through the worktree's link empties it) is no copy of
its key, so the next look prepares the other copy. A local lane's copy
(ADR-0044) is checked the same way before its service starts, prepared
again in place, and said in a notice.

## What this ADR does not say

- That the lane on the node uses this machine's store. It uses its node's.
- That the main working tree gets links. It has its own directories.
