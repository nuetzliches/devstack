# ADR-0088: A lane shows how far its branch is ahead of and behind its upstream, from the local refs only

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) (the branches and the Fetch on request; extended here), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (the hub's look at the worktrees; extended here), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (the programs a tree opens in), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The owner wants to see in the dashboard when a lane's branch and its
upstream differ, and to get from there to a git client. Whether `origin`
has moved is known only after a fetch; a fetch devstack runs by itself
would be a call to the remote in the user's name, with credentials and
possibly sign-in windows. The git clients a developer uses (GitHub Desktop,
an IDE) fetch by themselves, and every fetch leaves the remote-tracking
refs and `FETCH_HEAD` in the repository.

## Decision

The owner approved the points in the conversation on **2026-11-03**, all
five:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | devstack counts ahead and behind from the local refs only (`%(upstream:track)`) and never calls the remote for it; how fresh the numbers are is the last fetch's, whoever made it (a git client, the IDE, the command line, the branch picker's Fetch). | A fetch in the background: network, credentials, sign-in windows. | Medium: a background fetch would be a point of its own, with its own card. |
| 2 | concept | No "fetched … ago" label. | A label nobody reads. | Low: `fetched` is in the API already. |
| 3 | contract | `Branch` in `GET /hub/stacks/{id}/branches` gets the optional fields `ahead` and `behind`; absent without an upstream or when it is gone. | A new endpoint. | Low: optional fields. |
| 4 | contract | The hub's once-a-second look at the worktrees also takes in `FETCH_HEAD`, `packed-refs` and the ref files of the checked-out branches and their upstreams; a fetch, commit or push by any client moves the `worktrees` version and the dashboard reads again. git runs only when something changed. | git every second, or a file watcher of the hub's own. | Low: internal; the event type is the same. |
| 5 | concept | A click on the badge opens a menu of the programs of the kind `git` (ADR-0046) for that lane's folder, and "Manage programs…". | A list of git clients of its own. | Low. |

Decided while building, not on the card: the badge shows only when the
branch differs from its upstream (not at 0/0, not without an upstream); it
is in the lane's header only, not in the sidebar; its menu has no Fetch
(that stays in the branch picker).

As built:

1. **The count.** `branches::list` asks `for-each-ref` for
   `%(upstream:track)` with the other fields; `[ahead N, behind M]`, either
   alone or nothing (in step) give `ahead` and `behind`, `[gone]` neither.
   No other git call is added.
2. **The print.** `worktrees::start` looks at each registered stack once a
   second as before, and adds the modification times of `FETCH_HEAD`,
   `packed-refs` and the loose ref files of every checked-out branch and
   its upstream (`refs/heads/feat/x`, `refs/remotes/origin/feat/x`, a
   folder per segment). Which files those are it asks git
   (`git worktree list`, `for-each-ref refs/heads`) only when the trees'
   print changes: the `worktrees` folder, the main tree's `HEAD`, or the
   repository's `config` (a `git push -u` or `git branch -u` sets an
   upstream there). A file not there yet is looked at all the same: a
   first fetch or push makes it.
3. **The dashboard.** The stack store reads `branches` again whenever the
   stack's `worktrees` version changes, not only when the picker opens.
   The header shows `↑n ↓m` (only the parts that are not zero) beside the
   branch picker, for the shown tree's branch; its title names the
   upstream and says "as of the last fetch". Its menu lists the openers of
   the kind `git` the hub found for that folder, and "Manage programs…".
   A lane shared with us, someone else's stack, has no badge.
4. **Code.** `crates/devstack-hub/src/branches.rs` (`track`, `upstreams`),
   `crates/devstack-hub/src/worktrees.rs` (`ref_files`),
   `ui/src/components/StackHeader.vue`, `ui/src/stores/stack.ts`. Tests:
   `branches.rs` (parsing, a clone before and after a fetch),
   `worktrees.rs` (the ref files of a clone with a worktree),
   `stores.test.ts`.

## Consequences

- `↓0` means "nothing new as of the last fetch", not "nothing new on the
  remote". A user who wants the remote's state fetches, with a client or
  the branch picker.
- A branch whose upstream is set without touching `config` (none known)
  would be noticed at the next change of the trees.
- The branches are read on every change of a stack's refs: one
  `for-each-ref` and one `cat-file --batch-check` per change, not per
  second.
