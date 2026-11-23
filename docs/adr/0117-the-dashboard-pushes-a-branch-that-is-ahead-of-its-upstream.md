# ADR-0117: The dashboard pushes a branch that is ahead of its upstream, and the start page shows how far each stack is

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-23
- **Deciders:** project owner
- **Related:** [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) (point 8, "push is a Git client's", taken back for push; extended here), [ADR-0088](0088-a-lane-shows-how-far-its-branch-is-ahead-and-behind.md) (the badge, shown on the start page too; extended here), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (the git clients a path opens in), [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md), [ADR-0019](0019-control-api-security.md), [ADR-0108](0108-the-start-page-has-large-and-small-tiles-and-a-list.md) (the start page), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The lane's header shows `↑1` when its branch has a commit its upstream has
not (ADR-0088). The one thing a user does about it is push, and the badge's
menu led only to a git client for that. The start page, where a user sees all
stacks at once, said nothing of it: a commit left in a stack's main tree
showed only when its lane was opened. ADR-0047 point 8 left push to Git
clients; the hub had no `git push` anywhere.

## Decision

The owner approved the points in the conversation on **2026-11-23**, all
five:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | The hub runs `git push` for the user, on a click that was confirmed; that takes ADR-0047 point 8 back for **push** (commit, merge, rebase and force stay a Git client's). | Always opening the git client: a detour for the commonest handgrip. | Medium: remove the route and the button. |
| 2 | trust | The push is narrow: only `refs/heads/<branch>:<the upstream's branch>` to the branch's own upstream, never `--force`, never without an upstream, only when the hub reads `ahead > 0` and `behind = 0` from the local refs (read again at the click); no prompt (`GIT_TERMINAL_PROMPT=0`), 120 s; the user's git hooks run as for any push. | A bare `git push` (depends on `push.default`), `--force-with-lease`. | Low. |
| 3 | contract | `POST /hub/stacks/{id}/push {path}` (cookie and Origin): `path` is the stack's folder or a worktree git lists; `{}`, `400` (not pushable), `409` with git's message. | A branch name from the page instead of a folder. | Low. |
| 4 | contract | `StackEntry` gets an optional `sync {branch, upstream, ahead, behind}` for the branch of the main working tree; absent without an upstream or when level. The hub fills it from the refs it already looks at once a second (ADR-0088 point 4); no new polling, no network. | The start page reading `/branches` of every stack; an hourly or minutely poll like `last_commit`. | Low: one optional field. |
| 5 | concept | The start page shows `↑n ↓m` for each stack's main tree (tile and row), not for lanes and not in the sidebar or the stack's page. A click, with `behind = 0`, asks and pushes; behind the upstream (or both) it is a hint to bring it up to date in a git client. The header's badge behaves the same; the git client menu stays behind the path's menu (ADR-0046) and, behind the upstream, on the badge. | A push button beside the badge. | Low. |

Decided while building, not on the card: no fetch before or after the push
(a push moves `origin/<branch>` itself, which moves the `worktrees` version
and the badge goes); the question names the branch, the upstream and the
number of commits, and on the start page the stack, not the commits; a lane
shared with us has neither badge nor push; a refusal shows in the problem
dialog the other actions use; the command palette has "Push <branch>" for the
shown tree.

As built:

1. **The hub.** `branches::tree_state` reads a tree's branch and its
   `%(upstream:short)` and `%(upstream:track)` (`symbolic-ref`,
   `for-each-ref`); `sync_of` keeps it when it differs. `branches::push`
   resolves the folder with `worktree_of`, refuses a branch that is behind,
   level or without a remote branch, reads `branch.<b>.remote` and
   `branch.<b>.merge`, and runs `git push --porcelain -- <remote>
   refs/heads/<b>:<merge>` with the 120 s limit of a fetch. The thread of
   `worktrees::start` asks `sync_of` for a stack's repository only when that
   stack's version moved; `scan` copies it into `StackEntry.sync`.
2. **The page.** `SyncBadge.vue` (tile, row and header) is a button when the
   branch is ahead and not behind, else a hint. `lib/gitPush.ts` asks
   (`act` with a confirmation) and posts; `stack.shownSync` is the header's
   and the palette's figure. The mock hub has a stack ahead (it pushes), one
   behind.
3. **Code.** `crates/devstack-hub/src/branches.rs`, `worktrees.rs`, `lib.rs`;
   `ui/src/components/SyncBadge.vue`, `StackTile.vue`, `StackRow.vue`,
   `StackHeader.vue`, `CommandPalette.vue`, `ui/src/lib/gitPush.ts`,
   `ui/src/stores/stack.ts`. Tests: `branches.rs` (a clone of a bare remote:
   ahead, pushed, behind refused, no upstream refused, a lane's branch),
   `crates/devstack/tests/hub.rs` (the list's `sync`, the route's checks, the
   remote has the commit), `SyncBadge.test.ts`, `HomeView.test.ts`,
   `stores.test.ts`.

## Consequences

- devstack now writes to a remote in the user's name, once per click and
  confirmation. A remote that asks for credentials fails with git's message;
  nothing prompts from the background.
- A `pre-push` hook of the repository runs and may refuse; the hub shows its
  output.
- `↑n` is as of the last fetch (ADR-0088): a remote that moved since makes the
  push be refused by git, and the dashboard says so.
- The start page's figure is the main working tree's branch only: a lane's
  unpushed commits show in its header.
- Commit, merge, rebase, force, and a push of a branch without an upstream
  (`push -u`) stay a Git client's.

## Enforced by

- `crates/devstack-hub/src/branches.rs` (`push`, `tree_state`, the tests);
  `crates/devstack/tests/hub.rs` (`a_push_needs_the_hubs_own_page_and_reaches_the_remote`).
- `docs/dashboard-api.md` (`POST /hub/stacks/{id}/push`, `StackEntry.sync`).
