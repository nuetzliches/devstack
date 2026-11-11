# ADR-0102: The stacks list can order by last commit, hide stacks, and filter by stack or worktree

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-11
- **Deciders:** project owner
- **Related:** [ADR-0084](0084-the-sidebar-orders-stacks-by-name-or-last-opened.md) (the order; gains a third choice), [ADR-0086](0086-favourite-stacks-list-first.md) (favourites; its point 5, hiding, is decided here), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (the sidebar's stack rows), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (every stack's trees listed), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md); issue #377

## Context

ADR-0084 and ADR-0086 left hiding stacks (#377) open, and the order knew only
"by name" and "last opened", which says what this browser showed, not where
work happened. A user with about twenty stacks, many of them old, still
scrolls past most of the list to reach what is in progress, and has no way to
find a stack or a worktree by typing.

## Decision

The owner approved the points in the conversation on **2026-11-11**:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | `StackEntry` gets an optional `last_commit` (Unix seconds): the newest commit on any ref of the stack's repository, absent while unknown. | A client-side proxy: "last opened" is per browser and says nothing about work. | Low: one optional field, older hubs and pages ignore it. |
| 2 | concept | The order menu gains "Last edited first": favourites first, then the newest commit, a stack without one by name; a snapshot, as "last opened" is, so a commit moves no row while the page is open. | Replacing "last opened"; re-sorting live. | Low: one menu entry. |
| 3 | concept | Hiding is a per-browser list (`devstack.sidebar.hidden`), no hub state: a hidden stack leaves the list and waits under a "Hidden (n)" row at its end, with "Show again" in its menu. A running stack is never hidden. | A flag in the hub's registry (API, mock, documentation); favourites were kept in the browser on purpose. | The values are per browser; moving them to the hub later loses them. |
| 4 | concept | The "Stacks" label opens a filter field (a magnifier shows when it is pointed at) that matches stack name, repository folder and worktree names; a stack that matches only through a worktree shows open, with just those worktrees. | A permanent search box, which costs a row. | Low: UI code only. |
| 5 | term | "Hide" is not "Remove from the dashboard": hiding is reversible and keeps the stack registered; removing forgets it until it starts again. | One merged action. | A rename. |

Decided on its own, not on the card: the hub reads the commit time with
`git log -1 --all --format=%ct` once a minute on a thread of its own, never in
the one-second scan; a hidden stack that starts is listed again; the top-bar
switcher leaves hidden stacks out (unless one runs or is shown); hiding the
stack that is shown goes to the start page, as removing does.

As built:

1. **The commit time.** `crates/devstack-hub/src/lastcommit.rs` keeps a map
   from repository folder to time, refreshed every 60 s from the registry's
   repositories; `scan` only reads it and sets `StackEntry.last_commit`
   (`crates/devstack-hub/src/lib.rs`). `--all` covers every worktree's
   branch, remote branches included. Until the first read the field is
   absent. A repository without a commit, or a folder that is no repository,
   stays absent.
2. **The order.** `devstack.sidebar.order` may now be `edited`. The page takes
   a snapshot of the times when the choice is made, and a stack joins it
   when its time first becomes known (the first scan of a hub that just
   started has none); a commit to a stack already in it moves nothing until
   the choice is made again.
3. **Hiding.** `devstack.sidebar.hidden` (stack ids), shared by the sidebar
   and the switcher in the page. The "Hidden" row counts the hidden stacks
   and folds them open; they show dimmed, with the same menu. "Hide" is
   disabled for a running stack, with the reason in its tooltip. Removing a
   stack drops it from the list too.
4. **The filter.** Every word typed must be part of the stack's name or
   repository folder, or of a worktree's label (its branch, or its name);
   case does not matter. Esc closes the field and clears it. The filter
   covers the hidden stacks too, under their row. A worktree match opens
   the stack's worktrees whatever its fold state says, and lists only the
   matching ones.
5. **Code.** `ui/src/lib/stackOrder.ts` (`orderStacks`, `useHidden`,
   `filterStacks`), `ui/src/components/Sidebar.vue`,
   `ui/src/components/StackSwitcher.vue`, two icons in `icons/icons.ts`,
   the mock hub's `last_commit`. Tests: `stackOrder.test.ts`,
   `Sidebar.test.ts`, `lastcommit.rs`. The field is in
   `docs/dashboard-api.md`.

## Consequences

- Last edited follows commits, not uncommitted work: a stack someone edits
  all day without committing does not rise.
- A repository's refs are read once a minute per stack; on a slow disk with
  very many stacks this is the cost of the order, paid whether or not
  anyone uses it. The read ends after 30 s a stack, as the hub's other git
  calls do.
- Each browser has its own hidden stacks, as it has its own order and
  favourites. A stack hidden in one browser is listed in another.
- ADR-0084 gains a third choice, and its note that hiding stays open is
  answered here; ADR-0086 point 5 is decided here.
