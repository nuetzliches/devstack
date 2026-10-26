# ADR-0062: What `prepare` makes is kept in a store per machine and repository, keyed by its inputs

- **Status:** Accepted (2026-10-26); measured and built on the developer's machine, noted 2026-10-26; built on nodes, noted 2026-10-26
- **Date:** 2026-10-26
- **Deciders:** project owner
- **Related:** [#277](https://github.com/nuetzliches/devstack/issues/277), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a lane runs from a copy), [ADR-0028](0028-lane-images.md) (point 3), [ADR-0033](0033-a-node-decides-what-runs-on-it.md), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) (slots, the member's disk cap), [ADR-0044](0044-a-lanes-prepared-directories-can-be-linked-into-its-worktree.md), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md), [ADR-0063](0063-a-lane-on-a-node-links-its-worktree-to-a-copy-from-the-local-store.md)

## Context

`prepare` (`npm ci`) runs in every copy: each lane's, each build's snapshot.
It already skips itself when the copy prepared the same `run`, `cwd` and
`inputs` before (a mark per copy). Across copies nothing is reused: with one
lane per branch, a machine installs the same lockfile once per lane, minutes
each, although an identical `node_modules` sits in the next lane's copy.

The npm cache (`~/.npm`) does not help: it saves the download, not the
extraction, linking and install scripts, and `npm ci` deletes `node_modules`
before it installs, so a seeded copy followed by `npm ci` gains nothing. What
can be skipped is the whole run, when its inputs are the same.

A lane image (ADR-0028 point 3) does not help either: `prepare` runs in the
copy, a bind mount at `/src`, where nothing of the image shows, and `HOME`
is the slot's `home/`, mounted over the image's.

The result of a run depends on more than the files of `inputs`: native
packages differ between Windows and Linux, x64 and arm64, and between
Node versions (their ABI).

Tools write into what `prepare` made (`node_modules/.cache`, install
scripts, a dev server's own caches). Whatever is shared must not be
written by a lane.

## Decision

1. **`prepare` names what it makes:** `outputs`, directories relative to the
   repository (`outputs = ["web/node_modules"]`). Only a `prepare` with
   `outputs` uses the store; without, it runs as before. `link` (ADR-0044)
   must name a subset of `outputs` when both are given. The key is additive;
   a file that uses it raises `[stack] requires`, since older versions reject
   unknown keys.
2. **The key** of a result is the hash `prepare` keeps today (`run`, `cwd`,
   the paths and contents of the files `inputs` matches), plus the operating
   system and architecture, plus, in a lane container, the id of the lane
   image. `prepare.version` (`"node --version"`) is a command whose output,
   run in `prepare`'s `cwd`, is part of the key too; a stack without it
   accepts that a toolchain update on the same machine is not noticed. A
   `version` that fails disables the store for that run, with a notice.
3. **Where:** on a developer's machine, in the main working tree's
   `.devstack/store/`, shared by its builds and all its lanes. Their copies
   live under the same `.devstack/`, on the same volume, which a cheap copy
   (point 5) needs. On a node, per member and repository
   (`secret_name(node secret, ["store", member, repo])`, like a slot): a
   `prepare` runs the member's code, and its result never reaches another
   member's lane. Nothing of a store crosses machines; each machine fills its
   own.
4. **An entry is written once and never changed.** After a `prepare` that
   succeeded, its `outputs` go into a temporary directory in the store, which
   is renamed to the key and then marked complete. An entry without the mark
   is no entry. A key prepared by two copies at once is prepared once: the
   second waits for the first under a lock per key, and finds the entry. On
   a node the store is mounted read-only into the lane container; the lane
   hands a new entry over through its slot and the agent moves it into the
   store.
5. **A hit is copied into the copy**, and `prepare` does not run: the
   cheapest copy the filesystem allows (a reflink on btrfs and XFS, block
   cloning on ReFS and a Dev Drive, a clone on APFS), else a plain copy.
   What is copied is writable. No copy shares a directory or a hardlink with
   the store.
6. **Copy or run is decided by measurement.** devstack notes per key how long
   `prepare` took and how long its copy took. Where a plain copy took longer
   than the run, the next miss of that copy runs `prepare` instead (a plain
   copy of many small files on NTFS may be slower than `npm ci`).
7. **A miss runs `prepare` as today** and stores its result. A lane that
   changes the lockfile misses, makes a new entry and leaves the old one to
   the lanes that still use it.
8. **An output that is tracked by git is not stored**: a copy from the
   store would overwrite the source. The store is not used for that
   `prepare`, and a notice says why.
9. **Housekeeping.** Each hit notes when the entry was used. Locally an entry
   unused for 14 days goes (a sweep at daemon start and daily); `devstack
   lane prune --older-than <age>` removes entries older than `<age>` too. On
   a node, the member's store counts towards its disk cap and retention
   (ADR-0038 point 3), least recently used first, and goes with the member.
10. **It is said.** The diagnostic log and telemetry (ADR-0037) have
    `prepare_store_hit` and `prepare_store_miss`, with the key and the time
    taken; a lane's start says `prepare: from store`.

## Alternatives considered

- **One writable directory shared by running lanes.** Tools write into it; a
  lane would change what another runs.
- **Hardlinks.** Cheap, but a tool writing a file in place changes it in the
  store and every other copy.
- **The dependencies in the lane image** (ADR-0028 point 3). Only for nodes,
  one image build per lockfile change, and invisible in the copy without a
  step that copies them, which is this store with a registry in between.
- **A store per machine, across repositories.** Two repositories with the
  same lockfile are rare, and the store would leave the volume of a
  repository's copies, losing the cheap copy.
- **Near-miss reuse** (the closest entry, then `npm install`). Possible later;
  `npm install` may rewrite the lockfile and is less deterministic than
  `npm ci`.
- **Moving entries between machines** of the same platform. Tens of thousands
  of small files over the network gain little over `npm ci` with a warm npm
  cache.

## Consequences

- A second lane with the same lockfile starts without `npm ci`; on a
  filesystem with cheap copies in seconds.
- The disk holds each entry once more than today, until it goes: a lockfile
  that changes daily leaves an entry a day for 14 days.
- Without `version`, an update of the toolchain on a machine is not seen by
  the key. A stack whose `prepare` builds native code should set it.
- `outputs` has to name everything `prepare` makes that the service needs.
  What `prepare` writes elsewhere is not in a hit.
- Where a node's lane runs the agent must take entries over from it, a new
  path from a lane to its node besides the mirror.

## Enforced by

Where it will live:

- `crates/devstack-config/src/model.rs` (`Prepare::outputs`, `version`),
  `validate.rs` (inside the repository, literal, `link` ⊆ `outputs`), a
  frozen compat file in `crates/devstack-config/tests/compat/`.
- `crates/devstack-daemon/src/supervisor/copy.rs` (`prepare_job`: the key;
  hit, miss, store), a new `store.rs` (layout, lock, atomic write, copy,
  measurement, sweep); `artifacts.rs` (a build's `prepare`).
- `crates/devstack-daemon/src/lanes.rs` (`prune`).
- `crates/devstack-node/src/node.rs` (the member's store, its cap and
  retention, taking an entry over), `lanerun.rs` (the read-only mount, the
  image id for the key).
- `docs/stack-file.md` (`prepare`), `skills/devstack-config`.

*Measured, noted 2026-10-26.* A `node_modules` of 24,500 files (an
Angular 20 project with ESLint, Vitest, Playwright, Sass), `npm ci` with a
warm npm cache against a copy of what it made, on the same volume:

| Where | `npm ci` | copy, one file at a time | copy, 16 to 64 at once |
|---|---|---|---|
| Windows 11, NTFS | 23 s | 13 s | 5 s |
| Windows 11, Dev Drive (ReFS) | 43 to 51 s | 6 to 10 s | 2 to 3 s |
| Linux, XFS (a pool node, in a `node:24` container) | 2.6 s | 0.5 s | 0.5 s (reflink) |

The copy wins everywhere; on Windows the files, not the bytes, take the
time, so copying many at once matters more than block cloning. Point 6
stays as the guard. npm 11 runs install scripts only once allowed: a
project with allowed scripts takes longer to `npm ci`, not to copy.

*Paths as built, noted 2026-10-26.* `crates/devstack-daemon/src/store.rs`
(layout, `put` through a temporary directory renamed to the key with its
metadata in it, `copy_into` with 32 files at once and links moved to the
copy, the lock per key in the process, `sweep`);
`crates/devstack-daemon/src/supervisor/copy.rs` (`store_use`: `outputs`,
none tracked, not in a lane on a node; `store_key` with `version`;
`prepare_from_store`, `store_put`; `sweep_store` the first time the store is
written after a daemon starts, then daily, in the background);
`crates/devstack-daemon/src/lanes.rs` (`prune` with `--older-than`). Not
built: the store on a node (point 3's second half, point 4's last
sentence, point 9's node part), the lane start naming the hit in the
dashboard (`prepare-*.log` and the diagnostic log do).

*The store on a node, as built, noted 2026-10-26.* The store module moved to
`crates/devstack-build/src/store.rs`, shared by the daemon and the node
agent: `Store::with_outbox` (a lane reads the store and writes new entries
and notes of its hits into an outbox; links in an entry point to where it
is read from), `take_in`, `entries`, `remove`. The node agent
(`crates/devstack-node/src/node.rs`): a store per member and repository in
`<node>/stores/<secret_name(["store", member, repo])>` with a note of whose
it is (`prepare_store`); every round (15 s) the outboxes of the running and
kept lanes' slots are taken in (`take_in_stores`); the sweep removes entries
unused for the node's retention, counts the rest with the member's kept
lanes, evicts the store's least recently used entries first over the room,
and removes a gone member's stores (`sweep_slots`, `purge_gone_members`).
`crates/devstack-node/src/lanerun.rs`: the store mounted read-only at
`/store`, `DEVSTACK_STORE`, `DEVSTACK_STORE_OUT` (`/src/.devstack/store-out`)
and `DEVSTACK_LANE_IMAGE` (the image's id, part of the key). The lane's
daemon (`supervisor/copy.rs`, `node_store`) uses the store when the node
gives it one, and does not sweep it. Still not built: the hit named in the
dashboard (#291).

*Noted 2026-10-26 (#297).* Point 2's key covers the `outputs` as well, and
an entry is found under any service that made it: the same command in the
same directory with the same inputs and outputs makes the same, whichever
service names it (two services of one `node_modules`, a dev server and an
end-to-end tool). Entries made before have another key; the sweep removes
them.

## What this ADR does not say

- That the main working tree uses the store. It runs in place, with the
  developer's own `node_modules`.
- That a store entry is ever linked into a worktree. ADR-0063 links a copy.
- Anything about warming the store for a ref before a lane needs it.
