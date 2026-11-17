# ADR-0107: The start page has a search by name or folder and a filter by state

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-17
- **Deciders:** project owner
- **Related:** [ADR-0104](0104-the-start-page-shows-every-stack-as-a-tile.md) (the start page), [ADR-0102](0102-the-stacks-list-orders-by-last-commit-hides-stacks-and-filters.md) (the sidebar's filter), [ADR-0086](0086-favourite-stacks-list-first.md) (favourites), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The start page shows every stack as a tile (ADR-0104). With many stacks there
was no quick way to find one, or to see only the ones that run or need
attention; the sidebar's filter (ADR-0102) is a click away from it and
reaches stacks and worktrees, not the tiles.

## Decision

The owner approved the points in the conversation on **2026-11-17** (with the
plan that carried them), after choosing that the search covers name and
folder, that the state filter is part of it, and that the page keeps a field
of its own:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The start page has a search field above the tiles; every word typed must be part of the stack's name or repository folder, whatever the case. | Sharing the sidebar's field, which would change the sidebar without cause. | Low: UI code only. |
| 2 | concept | Three state filters, "All", "Running" and "Needs attention", each with its count, combine with the text. | A search field alone, which cannot narrow by state. | Low: UI code only. |
| 3 | concept | The search finds the stack itself, not its worktrees. | Searching worktrees too, which needs every stack's lanes on this page (the sidebar reads them); it can be added later. | Low. |
| 4 | term | Text and state are kept in this tab's session storage, so the way to a stack and back through the "devstack" mark finds them as left. | Starting empty each time. | Low. |

Decided on its own, not on the card: `/` focuses the field (unless a field
has the keys), Esc clears it and, once empty, leaves it; a hidden stack stays
out of the tiles, but while something is typed the line under them says how
many hidden stacks match, so a search never denies a stack that exists; a
filter with no match says so and offers "Clear the filter".

As built:

1. **The match.** `matchesQuery` (`ui/src/lib/stackOrder.ts`) is the stack's
   own part of the sidebar's `filterStacks`, which now uses it, so both read
   the words the same way.
2. **The state.** "Running" is a stack whose daemon runs, "Needs attention" a
   stack with an item on the `attention` event, as the counts in the page's
   head. The chips count the listed (not hidden) stacks, whatever is typed.
3. **The page.** `ui/src/views/HomeView.vue`; the text and the state are
   `homeQuery` and `homeState` in `ui/src/stores/ui.ts`, saved in the session
   storage as the sidebar's filter is.

## Alternatives considered

- **One field for the sidebar and the page.** One text for both, but typing
  on the page would filter the sidebar and the other way round.
- **Searching worktrees as well.** Matches what the sidebar finds, at the
  cost of reading every stack's lanes on the start page.

## Consequences

- A filter left on is still on when the page is opened again in the same
  tab; the head's figures count every stack, the chips the listed ones.
- A stack that needs attention only through its own state (no item on the
  `attention` event) is not counted by the chip, as it is not by the head.
- The page needs no new hub call.

## Enforced by

- `ui/src/views/HomeView.vue`, `ui/src/lib/stackOrder.ts` (`matchesQuery`),
  `ui/src/stores/ui.ts` (`homeQuery`, `homeState`); tests in
  `ui/src/views/HomeView.test.ts` and `ui/src/lib/stackOrder.test.ts`.
