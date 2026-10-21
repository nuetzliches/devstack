# ADR-0050: What no request reports reaches the dashboard as a notice

- **Status:** Accepted (2026-10-21)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [ADR-0021](0021-a-hub-serves-the-dashboard.md) (event streams, no polling), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (lanes), [ADR-0044](0044-a-lanes-prepared-directories-can-be-linked-into-its-worktree.md) (links into the worktree), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (desktop notifications), plan §11 (the diagnostic log)

## Context

The daemon and the hub write every decision to a diagnostic log (plan §11).
The dashboard does not show that log. Of the roughly 50 places that log a
warning or an error, about 20 also reach the user another way: an HTTP error
the dashboard shows as a toast, a service's status and reason, a lane's
`node_error`, a hub event. About 10 are devstack's own diagnosis (a retry
that succeeds, an accept loop's hiccup). The rest stay in the log, though
the user is left with something that does not work:

- a lane's worktree is not there, and in ten minutes the lane goes with its
  containers and volumes (`lane_worktree_missing`);
- a worktree gets no lane, because no port range is free (`lane_no_ports`);
- a lane's copy is not brought up to date, so its dev servers run old code
  (`copy_not_synced`);
- what `prepare` installed is not linked into the worktree, so its git hooks
  fail (`prepare_not_linked`);
- a lane on a node that served the main ports is gone, and the main tree's
  services did not all start again (`main_slot_lost`,
  `main_slot_back_failed`);
- a shared lane's tunnel is refused, and the browser shows a dead page
  (`shared_lane_tunnel`); a desktop notification is not shown, while the
  dashboard said it was sent (`notify_failed`).

They have one thing in common: they happen in a background loop (the lanes'
sync, the copy's debounce, a forward's accept), so there is no request whose
answer could carry them, and no field in a view says them.

Showing the log in the dashboard does not help. It is mostly routine lines,
the few that matter would drown in it, nobody looks there while nothing
seems wrong, and it is unreachable when the daemon does not answer, the one
time it is needed most. The log stays what it is: for `devstack logs
--daemon | --hub`, the MCP tool `diagnostics`, and agents.

## Decision

1. **A condition is a notice of the tree it concerns.** Something that holds
   for a while, and the user would want to know, is listed in the tree's
   `StackView.notices` while it holds, and leaves the list when it no longer
   does. A notice is `{id, level, text, since}`: `id` is stable while the
   condition holds (`copy`, `prepare.web.node_modules`), `level` is `warn`
   or `error`, `text` says what is wrong and what follows from it, `since`
   is when it began (unix ms). A `notices` event carries the tree's whole
   list whenever it changes.
2. **A lane's notices are in its `LaneView` too**, so the lane list can mark
   it while another tree is shown. A lane that does not run has no
   supervisor; what the main tree's daemon knows of it (its worktree is
   missing) is listed there all the same.
3. **An occurrence is a `notice` event.** Something that happened once,
   with no request to answer (a lane on a node gone, the main ports taken
   back), goes out as `{type: "notice", level, text}` on the stream of the
   tree it concerns, or on `/hub/events` for the hub's own. The dashboard
   shows it as a toast, and the Events page lists it.
4. **The log keeps its line.** Every notice and every occurrence is also a
   diagnostic line, as before, with the same event name and fields. A
   notice's `text` is written for a person, the log's fields for a search.
5. **A notice is the last resort, not the first.** What a request causes is
   said in its answer. What belongs to a service, a container or a lane's
   node has its field there (a status reason, `node_error`). A notice is for
   what has neither.
6. **The CLI shows them too.** `devstack status` prints the tree's notices
   as `!` lines, as it does for a changed stack file.
7. **Not a log view.** The dashboard does not show the diagnostic log, nor
   an excerpt of it filtered by level.

To begin with, notices are: a lane's worktree missing (with when the lane
goes), a worktree without a lane for want of ports, a lane's copy not
brought up to date, and `prepare` paths not linked. Occurrences are: the
main ports taken back from a lane that is gone, main services that did not
start again then, a shared lane's tunnel refused, and a desktop notification
not shown.

## Alternatives considered

- **The diagnostic log in the dashboard**, whole or its warnings and errors
  only. See the context: noisy, unread while nothing seems wrong,
  unreachable when the daemon is down. Its lines are written to be searched,
  not read one after the other.
- **A field per case** (`worktree_missing`, `copy_error`, ...). Each is
  small, but every new case changes the API, the CLI and the dashboard in
  three places, and the dashboard needs a place for each. Notices need that
  once.
- **Occurrences as notices** that the user dismisses. A dismissal is state
  the daemon would keep per user and browser; a toast and the Events page
  are enough for something that is over.
- **Desktop notifications for notices** (ADR-0045). The hub could notify
  of a new `error` notice like of a failed service. Left for later: first
  see which notices come up in practice, and how often.

## Consequences

- `StackView` and `LaneView` gain `notices`, the daemon's stream the events
  `notices` and `notice`, `/hub/events` the event `notice`
  (`docs/dashboard-api.md`). Older daemons leave them out; the dashboard
  reads them as empty.
- A new case is one call where the condition is found (`set_notice`,
  `clear_notice`) and one where it ends. Finding the end is the work: a
  notice that never goes is worse than none.
- The diagnostic log stays the full record; notices are a choice from it.
