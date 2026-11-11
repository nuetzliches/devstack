# ADR-0084: The sidebar orders the stacks by name or by last opened, a choice kept in the browser

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-02
- **Deciders:** project owner
- **Related:** [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (the sidebar's stack rows), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (every stack's trees listed), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md); issue #377

## Context

The sidebar listed every stack by name, and nothing moved when something
started or stopped (#374). With more than a handful of stacks, a user wants
the ones they work on near the top. Issue #377 asked for favourites, an
order of one's own (dragging, by name, last used) and hiding. Dragging was
not wanted; sorting the trees within a stack waits until this proves too
little.

## Decision

The owner approved the points in the conversation on **2026-11-02**:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The user chooses the order of the stacks in a menu beside `+`: by name (the default) or last opened first. | Dragging, favourites, an order by state. | Low: the menu goes, a stored choice is ignored. |
| 2 | concept | The order does not change while the page is open: last opened is taken when the page loads or the choice is made. | Re-sorting on every click, a row moving under the pointer. | Low: UI code only. |
| 3 | concept | There is no order by state: a start or a stop moves nothing (as since #374); the dot tells the state. | Running first. | One more entry in the menu. |
| 4 | contract | The choice and when each stack was last opened live in the browser's `localStorage`, as the fold state does. | A setting in the hub, per machine. | Moving it to the hub later loses the values per browser. |
| 5 | concept | The trees within a stack and "Shared with you" stay ordered by name. | An order per section. | Adding a choice per section. |

As built:

1. **The menu.** A button (↕) beside `+` in the "Stacks" heading opens a
   menu with "By name" and "Last opened first"; the current choice has a
   check mark. Choosing takes a new snapshot of the times.
2. **Opened.** A stack counts as opened whenever it is shown (its page or
   a lane's views), however it was reached: the sidebar, the switcher, the
   palette, a link. The time is noted at once, and used from the next load.
3. **Never opened.** A stack never opened in this browser, or added while
   the page is open, follows the opened ones, by name.
4. **Keys.** `devstack.sidebar.order` (`name` or `opened`) and
   `devstack.sidebar.opened` (stack id to milliseconds). Removing a stack
   from the dashboard drops its time.
5. **Code.** `ui/src/lib/stackOrder.ts`, `ui/src/components/Sidebar.vue`.
   Tests: `stackOrder.test.ts`, `Sidebar.test.ts`.

## Consequences

- Each browser has its own order; two browsers on one machine can differ.
- Last opened says what this browser showed, not what an agent or the CLI
  used: a stack an agent works on through MCP does not rise.
- Favourites and hiding (#377) stay open.

## Addendum 2026-11-11

[ADR-0102](0102-the-stacks-list-orders-by-last-commit-hides-stacks-and-filters.md)
adds a third choice, "Last edited first" (the newest commit, reported by the
hub), with the snapshot rule of point 2, and builds hiding.
