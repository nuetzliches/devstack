# ADR-0097: The dashboard's mark says what needs attention, in red or yellow

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-06
- **Deciders:** project owner
- **Related:** [ADR-0060](0060-the-tray-is-a-client-of-the-hub.md) (point 3: `/hub/summary`, what needs attention), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (what notifies), [ADR-0021](0021-a-hub-serves-the-dashboard.md) and [ADR-0073](0073-a-pool-tells-its-members-what-changed-over-one-stream.md) (streams, no polling), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (requests waiting for an admin), [ADR-0074](0074-a-pool-hands-out-skill-sets-the-hub-installs-them-for-the-agents.md) (skill sets waiting for an answer), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The tray shows in its icon's colour whether something needs attention
(ADR-0060 point 4): the hub's summary, made from the streams it follows to
notify (ADR-0045). The dashboard had no such sign of its own. A failed
service or a candidate held for `promote` in another stack, or in a lane
not shown, was seen only on that stack's page, and a browser tab gave no
sign at all. The dashboard cannot work it out itself: it follows the
stream of the stack it shows, not every stack's, and polling
`/hub/summary` is what ADR-0021 and ADR-0073 rule out.

The other apps of the nuts family turn their mark over to the family's nut
when the pointer is on it, and their favicon carries a red dot while
something runs, like an unread badge.

## Decision

The owner approved the points in the conversation on **2026-11-06**, all
four:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | The hub's event stream sends an `attention` event when a dashboard connects and on every change: `{type, items: [{stack, lane, service, text, detail, level}]}`. These are the items `/hub/summary` lists, read from the same board. | Polling `/hub/summary`, or working it out in the browser, which sees only the stack on screen | Low: the event is only added; an older dashboard ignores it |
| 2 | contract | An attention item carries a `level`: `error` (a service, a build, a stack's start or a lane's start failed) or `warn` (a candidate held for `promote`). `/hub/summary` has it too; its `state` stays as it is. | A severity guessed from the text | Low: one more field |
| 3 | concept | The dashboard shows a dot on the favicon and on the mark in its top bar: red for an `error` item or a hub that cannot be reached, otherwise yellow for a `warn` item, a pool request waiting for an admin, or a skill set waiting for an answer. With nothing to show there is no dot. | A count in the tab title, or one colour as in the tray | Low: only how it looks |
| 4 | concept | The mark flips to the nut of the nuts family on hover, and the favicon flips with it, as in the family's other apps. There is no motion when the user asks for reduced motion. | No easter egg | None |

Decided by the agent alone (not asked):

- The nut is Lucide's `nut` (ISC), the same as in the family's other apps,
  drawn in the mark's green on its dark tile.
- The favicon is made at run time as SVG text (a `data:` URL, which the
  page's CSP allows for images): the front or the nut, with or without a
  dot, not four files.
- A newer devstack is not a reason for the dot: the top bar's update button
  says it already.
- A lane whose start or move on a node failed is in the summary while it
  is so. It used to notify only (`lane_failed`, ADR-0045) and was missing
  from `/hub/summary`; point 2 names it, so it is added there too.
- The dot's colours are the dark theme's `--danger` and `--warn` on the
  favicon (its tile is dark in either theme), and the shown theme's in the
  top bar.

## Built

- `crates/devstack-hub/src/summary.rs`: `Level`; `Item.level`; the board
  says when a glance changed (`Board::subscribe`); `attention(&AppState)`
  lists every stack's items with the stack's id.
- `crates/devstack-hub/src/notify/rules.rs`: the glance gives `held` the
  level `warn`, failures `error`; `Lanes::attention` lists the lanes whose
  start or move failed and still is so; `notify/mod.rs` puts them into the
  main tree's glance.
- `crates/devstack-hub/src/lib.rs` (`hub_events`): the `attention` event,
  made again when a glance or the stack list changed and sent only when the
  items did.
- `ui/src/lib/brand.ts`: `attentionTone`, the favicon's SVG, the shared
  `markFlipped`; `ui/src/components/BrandMark.vue`: the mark with its back
  and the dot, in `TopBar.vue`; `ui/src/App.vue` keeps the favicon in step.
  `hub.attention` and `hub.skillsWaiting` in `ui/src/stores/hub.ts` (the
  sidebar's skill badge uses the latter now). The mock hub sends the event
  as well.

## Consequences

- Every dashboard tab shows, in its favicon, whether something in any
  stack failed or waits for the user, without the tray and without opening
  that stack.
- A hub older than the dashboard sends no `attention` event: the dot then
  shows only what the dashboard knows itself (the hub out of reach, pool
  requests, skill sets).
- Containers are not in it, as they are not in the summary (ADR-0060
  point 3).
