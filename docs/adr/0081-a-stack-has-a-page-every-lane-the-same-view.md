# ADR-0081: A stack has a page of its own; every lane, `main` included, has the same view

- **Status:** Accepted (card in the pull request)
- **Date:** 2026-10-30
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a worktree is a lane), [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) (the branch picker), [ADR-0067](0067-a-change-at-runtime-says-who-made-it.md) (who serves the main ports, set how), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md), [ADR-0076](0076-a-pool-machine-has-one-page-the-pool-area-has-tabs.md) (the URL style of the pool area), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (a stopped stack's worktrees), [ADR-0080](0080-the-main-tree-answers-as-a-lane-shared-is-deprecated.md) (the main tree answers as the lane `main`; `singleton`), [#387](https://github.com/nuetzliches/devstack/issues/387) (several stacks on one machine)

## Context

The dashboard showed a stack as its main working tree with its lanes
beside it. The main tree's view differed from a lane's in many places: its
header had Start and Stop stack and the daemon's pid, port and uptime, a
lane's had its own actions, Serve main ports, Move and Share; the start
card, the branch picker and the command palette each told the two apart.
What belongs to the stack (its daemon, the main ports, the services that
run once per machine) had no place of its own and sat in the main tree's
view. The lane shown was store state, not in the URL (`?lane=` was read
once and dropped).

ADR-0080 made the main tree answer as the lane `main`, so a client can
address every tree the same way. The daemon is unchanged: the main tree is
still its own tree.

## Decision

The owner approved the points in the conversation on **2026-10-30**:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | Every lane, `main` (the main working tree) included, has the same view and the same switches. The UI distinguishes only whether a lane holds the main slot (the fixed ports), not where it comes from. | A main tree's view of its own, and texts that address the main tree and a lane separately. | UI only. |
| concept | A stack has a page of its own above its lanes: its lanes, the main slot, its singleton services, need and usage (later: a port graph and a port shift, #387). | The stack's things in the main tree's view, and no place for what spans its lanes. | UI only; a route less. |
| contract | Stack-level actions (start, stop and restart the stack, give the main slot to lane X) live on that page; a lane's actions live in the lane view only. | Stack and lane actions mixed in one header. | UI only. |

What was built:

1. **Routes.** `#/s/<stack>` is the stack's page; `#/s/<stack>/l/<lane>/...`
   a lane's views, with the tabs as before (overview, a service, a
   container, infrastructure, commands, usage, activity), `main` for the
   main working tree. The store's lane follows the route, and the route the
   store when it shows another lane by itself (a lane made from a branch,
   `main` after a lane's worktree went). Older links still work:
   `#/s/<stack>?lane=x` and `#/s/<stack>/services/...`, `/infra...`,
   `/commands`, `/usage`, `/events` go to the same tab of the lane named,
   else of the lane shown last, else of `main`. The CLI's links
   (`devstack status`, `up`, `ui --link`), the notifications', the
   `devstack://open` route and the tray's use the new form.
2. **One lane view.** The main tree is a lane entry in the dashboard:
   name `main`, at the repository, its branch, offset 0, holding the main
   ports unless a lane does, started unless nothing of it runs. The header,
   the start card, the branch picker and the command palette treat it as
   any lane. Its actions map to what its daemon offers: Start is its
   `POST /v1/up`, Stop its `POST /v1/down?infra=true` (its services and
   containers; the daemon and the other lanes keep running), Restart and
   Rebuild restart (rebuild) each of its running services at the same time,
   as a lane's warm restart does, since the daemon has no restart of the
   whole main tree. Shown disabled, each with a one-line reason: Cold,
   Discard data, Remove worktree, Move, Share, a start in the pool and a
   profile other than the daemon's. *Serve main ports* is the same button
   in every lane that does not hold them; in `main` it takes them back. A
   `main` with nothing running shows the start card, as a lane not started
   does.
3. **The stack's page.** Its name, repository, the daemon's state (pid,
   port, uptime; idle; offline) and version, the restart banner (another
   devstack installed, a changed stack file); Start stack, Stop stack
   (every lane and the daemon, `POST /hub/stacks/{id}/down?all=true`, the
   confirmation listing the lanes that stop) and Restart stack; who serves
   the main ports, since when and set how, with a control to give them to
   another started lane or back to `main`; the lanes (`main` first, then by
   name: branch, here or the node, state, port offset, the main-ports mark,
   CPU and memory from each lane's own `/v1/usage`, read every 10 seconds),
   a row opening the lane's views; and the services that run once per
   machine. A stopped stack's page shows Start stack and its worktrees as
   the hub lists them. It follows the main tree's stream without making
   `main` the lane an older link opens.
4. **`ServiceView.singleton`**: the daemon says whether a service runs
   once per machine (`Service::singleton()`, `singleton = true` or the
   deprecated `lanes = false`); an older daemon omits it, and the page says
   it does not know.
5. **Sidebar, switcher, palette.** The sidebar's stack row opens the
   stack's page; the trees under it, `main` first and always listed, open
   their lane views on the tab shown last; the highlight follows the route.
   The stack switcher opens a stack's page and a lane's view. The command
   palette offers the stack's actions and its lanes on the stack's page,
   the lane's actions, views, services and commands in a lane view. The
   sidebar's per-stack menu keeps Start and Stop (every lane).

Decided by the agent alone (not asked): main's Restart and Rebuild as
per-service restarts of what runs; the main tree's Stop without a
confirmation, as a lane's; the stack page's "need" left at usage (no API
gives a stack's need without a pool); the main tree shown with the start
card when nothing of it runs; Move and Share shown disabled for `main` only
where the pool offers nodes, as they are shown for a lane only then; the
sidebar listing `main` under every stack, also one without worktrees.

## Consequences

- One view to change for every lane; the main tree's exceptions are a
  list of disabled items with their reasons, in one place
  (`ui/src/lib/laneActions.ts`).
- A link names the lane it shows; a bookmark and a notification open the
  same lane again, whichever was shown last.
- Stop stack means the whole stack now; stopping only the main tree is
  `main`'s Stop.
- The stack's page is where a port graph and a port shift for several
  stacks on one machine (#387) go.
