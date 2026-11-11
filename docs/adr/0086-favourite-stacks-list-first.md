# ADR-0086: Favourite stacks list first, marked by a star kept in the browser

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0084](0084-the-sidebar-orders-stacks-by-name-or-last-opened.md) (the order; its point 1 beat favourites, amended here), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (the sidebar's stack rows), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md); issue #377

## Context

ADR-0084 let the user order the stacks by name or by last opened and left
favourites and hiding (#377) open. With many stacks, the few a user works
on every day still move with the order: by name they sit wherever their
name puts them, by last opened they drop when another stack was looked at.
The owner asked for favourites that show first.

## Decision

The owner approved the points in the conversation on **2026-11-03**:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | term | A stack can be marked a favourite, by a star in its sidebar row or an entry in its menu; favourites list first, the others below, each part in the chosen order (name / last opened). | A third order "favourites first", a separate "Favourites" heading, dragging. | Low: the star and entry go, stored ids are ignored. |
| 2 | concept | Marking or unmarking moves the stack at once: it is the user's own act on that row; opening still moves nothing while the page is open (ADR-0084 point 2 stands). | Waiting for the next load. | Low: UI code only. |
| 3 | contract | Favourites live in the browser's `localStorage` (`devstack.sidebar.favourites`, a list of stack ids), as the order does; removing a stack from the dashboard drops it. | A setting in the hub, per machine. | Moving it to the hub later loses the per-browser values. |
| 4 | concept | The top-bar stack switcher also lists favourites first (then running, then by name, as before); the command palette stays as it is. | Sidebar only; the palette too. | Low. |
| 5 | concept | Hiding stacks (#377) stays open. | Building it now. | — |

As built:

1. **The star.** A star button sits in the stack's row before "…". Like
   "…" it shows on hover and focus (always on a touch screen); on a
   favourite it always shows, filled. The stack's menu has "Add to
   favourites" / "Remove from favourites".
2. **The order.** `orderStacks` sorts the favourites before the others and
   each part by the chosen order; a favourite never opened here follows
   the opened favourites, by name, as ADR-0084 has it for all stacks.
3. **The switcher.** A favourite carries a small filled star before its
   name and is listed first.
4. **Keys.** `devstack.sidebar.favourites` (stack ids). The sidebar and the
   switcher share one list in the page, so a star shows in both at once.
5. **Code.** `ui/src/lib/stackOrder.ts`, `ui/src/components/Sidebar.vue`,
   `ui/src/components/StackSwitcher.vue`. Tests: `stackOrder.test.ts`,
   `Sidebar.test.ts`.

## Consequences

- Each browser has its own favourites, as it has its own order.
- ADR-0084 point 1 no longer beats favourites; its other points stand.
- Hiding stacks (#377) stays open.

## Addendum 2026-11-11

Point 5 is decided by
[ADR-0102](0102-the-stacks-list-orders-by-last-commit-hides-stacks-and-filters.md):
stacks can be hidden, in the browser, and favourites list first in its new
order "Last edited first" too.
