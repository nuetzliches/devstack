# ADR-0108: The start page shows the stacks as large tiles, small tiles or a sortable list

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-17
- **Deciders:** project owner
- **Related:** [ADR-0104](0104-the-start-page-shows-every-stack-as-a-tile.md) (the start page, its tile), [ADR-0107](0107-the-start-page-has-a-search-and-a-state-filter.md) (search and state filter), [ADR-0086](0086-favourite-stacks-list-first.md) (favourites), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The start page (ADR-0104) shows every stack as a large tile. With many stacks
the large tiles take much room, and a column of figures to compare (which
stack was committed to last, which has the most lanes) is easier to read in a
table than in tiles.

## Decision

The owner approved the points in the conversation on **2026-11-17** (with the
plan that carried them), after choosing three views (large tiles, small
tiles, a list) and that the choice is kept in the browser:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The start page has three views of the same stacks: large tiles (as before), small tiles, a list. A switch beside the search chooses; the search and the state filter apply in each. | A page of its own for each view, which would repeat the filters. | Low: UI code only. |
| 2 | concept | The small tile is the same tile, closer together: name, state, folder, services, lanes, last commit and the first thing that needs attention; how long it has been up and the ports shift are left out. | A second tile component. | Low. |
| 3 | concept | The list is a table with the sortable column headers every table has: favourite star, name (folder below), state, services, lanes, up, last commit, attention, programs. Until a column is chosen the order is the tiles' (favourites first); a column's order applies in the list only. | One order for every view, which would take the sidebar's order from the tiles. | Low. |
| 4 | term | The views are "Large tiles", "Small tiles" and "List"; the choice is kept in this browser (local storage, as the sidebar's order and the favourites are), while the search text and the state stay with the tab (ADR-0107). | Keeping the view per tab. | Low. |

Decided on its own, not on the card: a click on a row opens the stack (the
name is a link, the star and the programs do not open it); the attention
column is the number of items, its tooltip lists them; the "Up" column goes
on a narrow screen; the stack's folder is the second line of the name cell; a
stack without its stack file shows a warning mark beside its name.

As built:

1. **The figures.** `ui/src/lib/stackFigures.ts` (`stackFigures`) takes what
   the tile worked out (state, services, lanes, since when it runs, last
   commit, items) into plain values, with the state's `rank` for sorting; the
   tile, the row and the sort read the same values.
2. **The tile.** `ui/src/components/StackTile.vue` has a `compact` prop for
   the small tile.
3. **The row.** `ui/src/components/StackRow.vue`, a `<tr>` with `FavoriteOpeners`
   and `StatusPill`.
4. **The page.** `ui/src/views/HomeView.vue`: the switch is `homeView` in
   `ui/src/stores/ui.ts` (`devstack.home.view`); the list sorts with
   `useSort` (`ui/src/lib/sort.ts`, kept as `devstack.sort.home`), by name,
   state (the most urgent first), services, lanes, up (the longest first),
   last commit (the newest first) or attention (the most first).

## Alternatives considered

- **A table as the only second view.** Less to build, but the small tiles keep
  the overview's look at a third of the room.
- **Sorting the tiles by a column too.** The tiles keep the sidebar's order
  (ADR-0102); a second way to order them would need a place to choose it.

## Consequences

- A column sort set in the list stays in this browser until it is turned back
  to the tiles' order (a third click on the header); the tiles do not follow
  it.
- The list scrolls sideways inside its frame where the screen is too narrow
  for its columns.
- The tile's figures moved out of `StackTile.vue`: a change to them is made in
  `stackFigures.ts` and shows in all three views.

## Enforced by

- `ui/src/lib/stackFigures.ts`, `ui/src/components/StackTile.vue`,
  `ui/src/components/StackRow.vue`, `ui/src/views/HomeView.vue`,
  `ui/src/stores/ui.ts` (`homeView`); tests in
  `ui/src/views/HomeView.test.ts` and `ui/src/lib/stackFigures.test.ts`.
