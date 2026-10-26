# ADR-0065: The store is warmed for a ref before a lane needs it

- **Status:** Accepted (2026-10-26); built, noted 2026-10-26
- **Date:** 2026-10-26
- **Deciders:** project owner
- **Related:** [#290](https://github.com/nuetzliches/devstack/issues/290), [#277](https://github.com/nuetzliches/devstack/issues/277), [ADR-0062](0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md) (the store, its key), [ADR-0063](0063-a-lane-on-a-node-links-its-worktree-to-a-copy-from-the-local-store.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md), [ADR-0033](0033-a-node-decides-what-runs-on-it.md), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0023](0023-stack-file-compatibility.md)

## Context

The store (ADR-0062) saves every `prepare` after the first with the same
inputs. The first still pays: after a lockfile change on the main branch,
the first lane of each developer, and the first on each node, runs the full
`npm ci` (23 to 45 s measured on Windows for a mid-sized `node_modules`,
minutes on a large one), at the moment someone waits for that lane.

The inputs of the next lanes are known before any of them starts: they
branch from the main branch, and the developer's clone has fetched it. What
the store needs is the key of that ref's inputs, prepared once, ahead.

The key must be the one a lane's copy gets. A copy hashes the files as git
checked them out, after line-ending conversion and filters
(`core.autocrlf`, `.gitattributes`): hashing the blobs of the ref would give
another key on a Windows clone with `autocrlf`, and the warmed entry would
never be hit.

## Decision

1. **The stack file names the refs**: `[prepare] warm = ["origin/main"]`.
   Without it, nothing is warmed. An additive key; a file with it says
   `requires = "0.24.0"`.
2. **devstack watches the refs, it never fetches them.** The main tree's
   daemon reads each ref's commit every minute (`git rev-parse`). A new
   commit, unchanged for two minutes (a fetch, a pull, a merge done), is
   warmed; the commit it warmed last is noted in its state, so a daemon
   restart does not warm it again.
3. **The ref is checked out as git checks it out, beside the repository**:
   into a temporary directory under `.devstack/warm/`, with an index of its
   own (`GIT_INDEX_FILE`, `read-tree`, `checkout-index`), so the main tree's
   index, its worktree and the list of worktrees stay untouched (no lane is
   made of it). First only the files of each service's `inputs`: their key
   is computed as a copy's (ADR-0062 point 2, this machine's platform and
   `version`). A key the store has costs nothing more.
4. **A missing key is prepared in that checkout**, the whole ref checked
   out then, and its `outputs` go into the store as any `prepare`'s do
   (ADR-0062 point 4). It runs under the main tree's one-`prepare`-at-a-time
   lock, so it never runs beside a lane's or a build's; a lane that needs the
   same key meanwhile waits for it and copies the entry (the key's lock).
   The checkout goes afterwards. A failure is logged (`prepare_warm_failed`)
   and noticed once per commit; nothing retries it before the next commit.
5. **The nodes the repository's lanes use are warmed too**, for this
   member: the nodes its lanes ran on (ADR-0038 point 4 notes them), while
   one of its lanes is kept or runs there. devstack asks each for a **warm
   lease**: a lease like a lane's (ADR-0026: placed by the requester,
   admitted by the node, under the member's identity and the node's
   policy), with the checkout of point 3 as its mirror, which the node's
   lane container runs with only `prepare` of the services whose key the
   member's store there lacks (ADR-0062 points 3, 4), then ends. Its slot
   is the ref's, kept and swept like a lane's, so the next warm syncs only
   the difference. A node refuses it as it refuses any lease, and its
   policy can refuse warm leases alone: `warm = false` (default `true`).
6. **It is said.** `prepare_warmed` (service, key, ref, commit, where, `ms`)
   and `prepare_warm_failed` in the diagnostic log and telemetry; the store
   view of the dashboard (#291) shows an entry as warmed.

## Alternatives considered

- **Hash the ref's blobs** (`git ls-tree`, `git cat-file`): no checkout,
  but the wrong key wherever git converts on checkout (Context).
- **`git worktree add` for the ref**: the exact checkout, but a worktree is
  a lane to devstack (ADR-0024), and to the developer's tools.
- **devstack fetches**: it would change the developer's remote-tracking refs
  on its own schedule; whoever fetches decides when the main branch moved.
- **Warm on every node of the pool**: other members' nodes run work for a
  repository that may never run there; the nodes this repository's lanes
  use are where the next lane goes (ADR-0026 point 3 prefers them).
- **A node warms by itself from a kept slot**: the slot holds a branch's
  mirror, not the ref, and the node would run a member's `prepare` without
  the member asking (ADR-0033).
- **Near-miss reuse instead** (the closest entry, then `npm install`): kept
  out by ADR-0062.

## Consequences

- After a lockfile change on the main branch, the developer's machine runs
  `prepare` once in the background, with its CPU and disk, minutes after
  the fetch; each node of the repository runs it once more for its
  platform. A stack that does not want that has no `warm`.
- The store holds the main branch's entry as well as the lanes': one more
  entry per lockfile change until it goes (ADR-0062 point 9).
- A warm lease takes a node's capacity like a short lane: placement counts
  it, and the pool area lists it while it runs.
- A ref whose `prepare` needs files outside the ref (`[lanes] sync`, a
  `.env`) gets them as a lane on a node does: the main tree's `[lanes]
  sync` files go along.

## Enforced by

Where it will live:

- `crates/devstack-config` (`[prepare] warm`, `WARM_SINCE`, a frozen compat
  file with the release).
- `crates/devstack-daemon/src/supervisor/copy.rs` (the key over a checkout,
  `run_prepare` for it), a new `warm.rs` (watching the refs, the checkout,
  the note of the last commit, asking the nodes).
- `crates/devstack-daemon/src/remote.rs` (a warm lease: placed, synced,
  ended), `crates/devstack-node` (a lease of kind `warm`: the container runs
  `prepare` only; `warm` in the node policy).
- `docs/stack-file.md`, `docs/pool.md`, the store view of #291.

*Paths as built, noted 2026-10-26.* `crates/devstack-config`
(`PrepareSettings::warm`, validated as a ref devstack hands to git: no
option, no range, no whitespace; an error without a service whose `prepare`
names `outputs`; `WARM_SINCE`). `crates/devstack-daemon/src/warm.rs`:
`Watch` (every minute, two minutes quiet, the last commit warmed in
`.devstack/warm/done.json`), `Checkout` (`read-tree`, `checkout-index` with
`GIT_INDEX_FILE`; inputs first, `completer` for the rest), `warm_here`,
and in a warm lease's container `run_lane` and `GET /v1/warm`.
`supervisor/copy.rs`: `warm_service` (the key over the checked-out inputs),
`warm_lane`. `lanes.rs`: `follow_warm`, `warm_nodes` (the nodes of
`ran_on` whose slots hold a lane of the repository; a checkout of its own,
without this machine's outputs; the lane name `warm--<ref>`). `remote.rs`:
`RemoteLane::warm` (lease with `warm`, a node before 0.24.0 refused, sync,
start, `GET /v1/warm` until done, release). `crates/devstack-node`:
`warm` in `LeaseRequest`, `Lease`, `NodeLease`, `NodePolicy` (`false`
anywhere wins); `lanerun.rs` makes no Compose group for a warm lease and
sets `DEVSTACK_WARM`.

## What this ADR does not say

- That devstack fetches or pulls, or checks out a ref in the developer's
  worktree.
- That a warmed entry is any different from a lane's: it is keyed, kept,
  copied and swept the same.
