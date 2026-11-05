# ADR-0095: The daemon ends with the last tree; a stack has no start of its own

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-05
- **Deciders:** project owner
- **Related:** [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (a lane's start brings the daemon up without the main tree), [ADR-0080](0080-the-main-tree-answers-as-a-lane-shared-is-deprecated.md) (the main tree answers as the lane `main`), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (a stack's page; its *Start* and *Stop stack*), [ADR-0093](0093-the-main-tree-takes-its-profile-when-it-starts.md) (point 6: *Start stack* starts the daemon and `main`), [ADR-0068](0068-a-stop-takes-the-containers-too.md) (point 2: keeping the containers; amended here), [ADR-0082](0082-the-ports-of-a-machine.md) (the port shift), [ADR-0052](0052-a-restart-carries-the-lanes-over.md) (a restart carries the lanes over), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (each lane has its own containers), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

Since ADR-0078 a lane's start brings the stack's daemon up, and since
ADR-0093 the main tree's profile comes with its own start, not with the
daemon's. *Start stack* then only started `main`, and `main`'s start card
did the same with a profile. *Stop stack* stopped every tree and the
daemon. Between them the daemon was a thing of its own the user had to
think about: a lane's stop left a daemon that ran nothing until someone
pressed *Stop stack*, and the dashboard asked "Stop the stack too" when
the last lane stopped.

Containers are a tree's, not the stack's (ADR-0030): a lane's stop always
took its containers. Only the stack's stop offered to keep them
("Keep its containers running", ADR-0068 point 2), so `main` behaved
differently from every lane.

## Decision

The owner approved the points in the conversation on **2026-11-05**, all
seven:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The daemon has no start or stop of its own: the first tree that starts (`main` or a lane, here or on a node) brings it up; once the last one stopped, it ends by itself, decided inside the daemon. | A daemon that runs nothing until *Stop stack* | Low |
| 2 | concept | What keeps it: a tree with a service or a container up (or an `up` under way), a lane on a node or one starting or moving, a command that runs. | A daemon that ends under a lane on a node or a seed | Low |
| 3 | contract | A daemon that ends answers what would start something with `503` and `"ending": true`; the CLI, the MCP server and the hub wait for it and start a new one. | Starts lost while it ends | Low |
| 4 | term | The stack's page has no start button: every tree, `main` included, starts on its own card. The port shift is offered on `main`'s card once its start was refused for the ports. | A button that starts `main` and sounds like more | Low |
| 5 | term | *Stop stack* becomes *Stop all* (every tree, the daemon ends with the last); *Restart stack* becomes *Reload* (the installed devstack and the current stack file, the lanes carried over). | Buttons that name a process | Low |
| 6 | contract | A stop always takes a tree's containers; the dashboard offers no choice. `--keep-infra`, `keep_infra` and `infra=false` keep working and are reported deprecated. This amends ADR-0068 point 2. | A box only `main` had | Low |
| 7 | term | In the sidebar a stack folds away once nothing of it runs any more. | An open stack in which nothing runs | Low |

Decided while building, not on the card:

- Only a daemon that ran something ends: one just started for a tree is
  not ended before that tree's start arrives. A request that may start
  something holds it while it is answered and for five seconds after: every
  request but a `GET`, and any request of the CLI or `devstack mcp`
  (`X-Devstack-Client: cli | mcp`), whose look at the daemon comes before
  what it asks of it. The dashboard's and the tray's reads do not: an open
  page does not keep a daemon that runs nothing.
- A failed or exited service keeps the daemon: its logs are still to be
  read, and `Stop` on it is the user's word that it is over. A finished
  job does not.
- A stop (`…/down`, `…/stop`, `/v1/shutdown`, a run's `cancel`) is still
  answered while the daemon ends.
- A daemon on a node (its agent's, for one lane) does not end this way:
  its agent ends it.
- During a restart (ADR-0052) the daemon does not end of itself.
- The CLI's probe takes a `503` with `"ending"` for no daemon and waits
  until it says `stopping`; `ensure_daemon` then waits for it to go and
  starts a new one, as after a `down`.
- The last lane's stop asks nothing more: the confirmation that offered
  "Stop the stack too" is gone. A reload does not fold the stack away.

## Consequences

- A stack in which nothing runs has no process; the next tree's start
  brings one up, as it did after *Stop stack*.
- `main` and a lane are stopped the same way: with their containers.
- The dashboard's *Start stack* is gone from the stack's page, the
  sidebar's menu and the command palette (there: *Start main*).

## Enforced by

`crates/devstack-daemon/src/idle.rs` (the decision, the middleware and its
`503`), `crates/devstack-daemon/src/lib.rs` (the look every second),
`crates/devstack-daemon/src/supervisor/lifecycle.rs` (`runs_anything`,
`up` counted), `crates/devstack-daemon/src/lanes.rs` (`any_running`),
`crates/devstack-daemon/src/commands.rs` (`Runs::any_running`),
`crates/devstack/src/client.rs` (`Client::probe`),
`crates/devstack/src/dispatch/stack.rs` (`--keep-infra` deprecated),
`crates/devstack/src/mcp.rs` (`down`), `ui/src/views/StackPage.vue`,
`ui/src/components/LaneStart.vue` (the shift), `ui/src/components/Sidebar.vue`
(folding), `ui/src/stores/stack.ts` (`stopStack`, `stopMain`, `stopLane`).
