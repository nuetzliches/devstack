# ADR-0109: The order of the stacks is one choice for the sidebar and the start page

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-17
- **Deciders:** project owner
- **Related:** [ADR-0084](0084-the-sidebar-orders-stacks-by-name-or-last-opened.md) (the order menu), [ADR-0102](0102-the-stacks-list-orders-by-last-commit-hides-stacks-and-filters.md) (last edited first), [ADR-0086](0086-favourite-stacks-list-first.md) (favourites first), [ADR-0104](0104-the-start-page-shows-every-stack-as-a-tile.md) (its point 3: the tiles follow the sidebar's order, read when the page opens), [ADR-0108](0108-the-start-page-has-large-and-small-tiles-and-a-list.md) (the list), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The start page took the sidebar's order when it opened (ADR-0104), but had no
way to change it: the choice had to be made in the sidebar's menu, and the
page showed it only after it was opened again.

## Decision

The owner approved the points in the conversation on **2026-11-17**, after
choosing that the order is one setting for both:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The start page gets the sidebar's order menu (by name, last opened first, last edited first; favourites always first) as a button in the filter row. | A menu of its own with other entries. | Low: UI code only. |
| 2 | concept | The order is one setting for the sidebar and the start page, and a choice in either shows in both at once. | Two separate orders. | Medium: the state lives in one shared module; separating them splits it again. |
| 3 | concept | The menu sets the order of the tiles and of the list; in the list a column chosen on its header overrides it until it is turned back. | The menu for the tiles only. | Low. |

Decided on its own, not on the card: the menu is a component both pages use
(`StackOrderMenu.vue`), each drawing its button in its own look; last opened
and last edited stay snapshots that a page takes when it opens and when an
order is chosen (also the one already chosen), so nothing moves under the
pointer.

As built:

1. **The choice.** `useOrderChoice` (`ui/src/lib/stackOrder.ts`) holds the
   order in one place, written to `devstack.sidebar.order` as before and read
   from the browser when a page opens; every choice also counts up, which the
   pages watch to take their snapshots anew.
2. **The ordering.** `useStackOrdering` takes the snapshots of one page and
   sorts a list with them; the sidebar and the start page both use it, the
   copy of that code in each is gone.
3. **The menu.** `ui/src/components/StackOrderMenu.vue` is the menu with its
   outside-click and Escape handling, the button comes from the page
   (`data-testid` `sidebar-order`, `overview-order`).

## Consequences

- The tiles of the start page follow a choice made while it is open; ADR-0104's
  "when the page opens" no longer holds for the order (it holds for the
  snapshots).
- A choice in one browser tab does not reach another tab until it is opened.

## Enforced by

- `ui/src/lib/stackOrder.ts` (`useOrderChoice`, `useStackOrdering`),
  `ui/src/components/StackOrderMenu.vue`, `ui/src/components/Sidebar.vue`,
  `ui/src/views/HomeView.vue`; tests in `ui/src/lib/stackOrder.test.ts`,
  `ui/src/views/HomeView.test.ts` and `ui/src/components/Sidebar.test.ts`.
