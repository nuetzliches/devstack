# ADR-0104: The start page shows every stack as a tile, with the hub's figures and the favourite programs

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-13
- **Deciders:** project owner
- **Related:** [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (the programs and their favourites), [ADR-0060](0060-the-tray-is-a-client-of-the-hub.md) (`/hub/summary`), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (a stack's page), [ADR-0086](0086-favourite-stacks-list-first.md) and [ADR-0102](0102-the-stacks-list-orders-by-last-commit-hides-stacks-and-filters.md) (favourites, order, hiding), [ADR-0097](0097-the-dashboards-mark-says-what-needs-attention.md) (the `attention` event), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The dashboard's `/` was only a redirect to the stack shown last, so the
"devstack" mark in the top bar, a link to `/`, seemed to do nothing. With
several stacks there was no place that showed all of them at once: the
sidebar shows dots, a stack's page one stack. The owner asked for a start
page with a tile per stack, a few figures, and the favourite programs of
"Manage programs…".

The hub already knew every stack's figures for the tray (`/hub/summary`,
ADR-0060): state, the main tree's services ready of all, lanes started,
what needs attention. The dashboard read only the attention items of it, as
the `attention` event (ADR-0097).

## Decision

The owner approved the points in the conversation on **2026-11-13** (with
the plan that carried them), after choosing that `/` is always the overview
and that the figures are what the hub already knows:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | `/` is the overview of every stack and no longer jumps to the last shown stack; the brand and opening the dashboard land there. | A separate `/stacks` route beside the old redirect. | Low: the redirect comes back in `HomeView`. |
| 2 | contract | `/hub/events` gains a `summary` event, `{stacks: [{id, state, ready, total, lanes}]}` as in `/hub/summary`, sent first and whenever it changed; older pages ignore it, older hubs never send it. | The page reading `/hub/summary` on every `attention` event (misses changed counts), or each stack's lanes (one call per stack). | Low: an event nobody else reads. |
| 3 | concept | The tiles follow the sidebar's order, favourites first, and leave hidden stacks out (a line says how many). | An order of their own. | Low: UI code only. |
| 4 | term | The favourite programs show as buttons on each tile and open the stack's main tree; they stay the browser's choice (`devstack.openers`). | Opening a lane's tree; a program list per tile. | Low: buttons go. |

Decided on its own, not on the card: the tile's name is the link to the
stack's page and covers the tile, the buttons sit above it; a tile shows up
to two attention items, then "+n more"; it shows how long the daemon has
been up and the last commit (in days once older than a day); the star on a
tile marks the stack a favourite, as the sidebar's does; with no stack, the
"Find your stacks" card stays; the browser no longer keeps the last shown
stack (`devstack.lastStack`), which only the redirect read.

As built:

1. **The event.** `summary::glances` (`crates/devstack-hub/src/summary.rs`)
   is `summary()`'s stacks without their items; `hub_events`
   (`crates/devstack-hub/src/lib.rs`) sends it on the triggers of
   `attention` (a glance on the board changed, or the stack list), only when
   it changed. A start or stop the hub runs (its phase) moves neither, so it
   shows once the daemon came or went; the page that asked for it shows
   "Starting"/"Stopping" from its own state meanwhile.
2. **The page.** `ui/src/views/HomeView.vue` orders with `orderStacks`
   (`ui/src/lib/stackOrder.ts`) by the order saved for the sidebar, read when
   the page opens; last opened and last edited are snapshots, as in the
   sidebar. A running stack is never left out, hidden or not.
3. **The tile.** `ui/src/components/StackTile.vue`: state from the glance,
   else from the list (an older hub); figures; the stack's items from
   `hub.attention`.
4. **The programs.** `ui/src/components/FavoriteOpeners.vue`, taken out of
   `PathMenu.vue`, which uses it too.

## Alternatives considered

- **A `/stacks` route, `/` still a redirect.** Keeps the landing on the last
  stack, but the mark would need a second meaning; the owner chose the
  overview as the landing.
- **CPU and memory per stack.** One `/v1/usage` read per running stack, on
  a page meant to be a glance; left out by the owner's choice.
- **Reading `/hub/summary` when `attention` arrives.** No hub change, but a
  service that comes up changes no attention item, so the counts would lag.

## Consequences

- The dashboard opens on the overview, one click further from the stack
  worked on last; the sidebar and the switcher still reach it in one.
- Every dashboard gets a `summary` event whenever a stack's figures change,
  a few hundred bytes, whether or not the start page is shown.
- The tiles' order follows the sidebar's choice when the page opens, not
  while it is shown.

## Enforced by

- `crates/devstack-hub/src/summary.rs` (`StackGlance`, `glances`),
  `crates/devstack-hub/src/lib.rs` (`hub_events`); tests in `summary.rs` and
  `crates/devstack/tests/hub.rs` (`pairing_cookie_and_checks`).
- `ui/src/views/HomeView.vue`, `ui/src/components/StackTile.vue`,
  `ui/src/components/FavoriteOpeners.vue`, `ui/src/stores/hub.ts`
  (`glances`), `ui/src/api/types.ts` (`StackGlance`), the mock hub
  (`ui/src/api/mock/index.ts`); tests in `ui/src/views/HomeView.test.ts`.
- `docs/dashboard-api.md` (the `summary` event).
