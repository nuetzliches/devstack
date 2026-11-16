# ADR-0105: A service can be a shortcut card on the overview, marked in the stack file

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-16
- **Deciders:** project owner
- **Related:** [ADR-0023](0023-stack-file-compatibility.md) (a new key is optional), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (the stack's page, every lane the same view), [ADR-0101](0101-a-services-link-can-come-from-its-output-and-stays-on-this-machine.md) (a service's link), pull request #536

## Context

A stack with many services (a downstream .NET + Angular stack has some
thirty, in six groups) shows its front end as one row among them. The
developer who opens the dashboard mostly wants one thing: open the front
end. A pin brings the row to the top, but a pin lives in one browser
(`ui/src/lib/pins.ts`): every developer, and every new browser, sets it
again, and nothing in the repository says which service is the way in. A
`[groups]` entry orders, but does not stand out.

## Decision

The owner approved the review card in the conversation on **2026-11-16**,
points 1 to 4, and added that several services may carry the key:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A service takes the optional key `shortcut = true`; absent, nothing changes. `SHORTCUT_SINCE` with its warning. | A pin (per browser, not in the repository); a `[groups]` entry (orders, no emphasis) | A published key goes only as a deprecated alias (ADR-0023) |
| 2 | term | The overview has a level "shortcut": a card of its own, first, with a prominent button that opens the service. | Making every pin larger | Text and layout only |
| 3 | concept | The card is a view of the service, not a second service: its state, its link; it starts and stops nothing. | A `[shortcuts]` table with names and addresses of its own | Medium: a second table needs a compatibility of its own |
| 4 | contract | The card opens the service's existing link (a printed `link`, else `url`, else its port), no address key of its own. | A key `open = "…"` | Low |

As built:

1. **The key** (`crates/devstack-config`): `Service::shortcut:
   Option<bool>`; `SHORTCUT_SINCE = "0.48.0"` and its warning in
   `validate::warnings`. Any number of services may set it.
2. **The view** (`crates/devstack-daemon`): `ServiceView::shortcut`, `true`
   where the stack file says `shortcut = true`, set when the supervisor is
   made; older daemons omit it.
3. **The card** (`ui/src/components/ShortcutCard.vue`, `OverviewView.vue`):
   the services with `shortcut` as cards in a grid above the pinned
   section and the groups, in the order the daemon lists them (by name).
   Each shows the service's name, its state in the state's
   colour, its port, and a large button to `serviceUrl` (the same link as
   its row); a service without a port or link says so instead. The state
   filter of the overview leaves the cards standing. The service stays in
   its group and can be pinned as before.

Decided while building, not on the card:

- Several shortcuts list by name, as the daemon lists the services (the
  stack file's `services` are a map); the card cannot be hidden or folded.
- A lane shows the same cards as the main tree (its own ports).
- The compatibility fixtures are unchanged: the key is optional.

## Alternatives considered

- **A pin set from the stack file** (`pinned = true`): pins stay a
  personal order; a shortcut is the stack's statement of where to start,
  and looks different.
- **A shortcut for a Compose container**: its links are `urls` of its
  group; no use named one yet.

## Consequences

- A stack file with the key fails on a devstack before 0.48.0 ("unknown
  field") unless it says `requires = "0.48.0"`.
- A shortcut to a stopped service shows its state and still opens its
  address; there is no start button on the card.

## Enforced by

- `crates/devstack-config/src/model.rs` (`Service::shortcut`), `lib.rs`
  (`SHORTCUT_SINCE`), `validate.rs` (warning),
  `tests/stack_files.rs` (`shortcut_may_mark_several_services`)
- `crates/devstack-daemon/src/state.rs` (`ServiceView::shortcut`),
  `supervisor/mod.rs`
- `ui/src/components/ShortcutCard.vue` (with its test),
  `ui/src/views/OverviewView.vue`, `ui/src/api/types.ts`
- `docs/stack-file.md`, `docs/dashboard-api.md`

## What this ADR does not say

- That a shortcut starts its service or keeps it running.
- That the card is shared beyond this machine's dashboard: a lane shared
  with the pool shows its services as before.
