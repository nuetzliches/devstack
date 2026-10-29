# ADR-0078: The hub follows every stack's worktrees; a stopped stack's lane can be started from the dashboard

- **Status:** Accepted (2026-10-29)
- **Date:** 2026-10-29
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a worktree is a lane), [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md) (lanes made from a branch), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (no polling), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

Two gaps, found in use:

- A worktree made with `git worktree add` in a stack that was not shown
  appeared in the dashboard's sidebar only when that stack was opened. The
  stack's daemon noticed it within a second and told its stream, but the
  dashboard follows the stream of the stack it shows only; the other
  stacks' lanes were read when the hub's list of stacks changed.
- A stack whose daemon did not run showed no worktrees at all. A worktree
  the dashboard made for it from a branch was listed in the branch picker
  and could neither be opened nor started: a lane's views and its start
  need the daemon.

## Decision

The owner approved the review card in the conversation on **2026-10-29**,
all three points:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| contract | The hub follows `.git/worktrees` of every registered stack, running or not, as the daemon does, and tells `/hub/events` when one changes; the dashboard reads that stack's lanes again then. | Reading lanes only when the list of stacks changes (before); following every running daemon's stream (a connection per stack, and nothing of a stopped one). | An additive event; without it the behaviour before. |
| concept | A stopped stack shows its worktrees in the sidebar as not started, read from git through the hub; a click opens the lane's start page. | A stopped stack's worktrees invisible and unreachable. | UI only. |
| contract | `POST /hub/stacks/{id}/lanes/start {path, on?}`: the hub starts the lane of a stopped stack as `devstack up` in its worktree does: the daemon comes up, the main working tree's services stay stopped, the lane starts here or on the pool. | "Start stack" first (the main tree's services), then the lane. | A hub route less; the stack is started first again. |

What was built:

1. **Following.** A thread of the hub looks once a second at
   `<git common dir>/worktrees` of each registered stack: its entries and
   their `HEAD`s (`devstack_daemon::lanes::fingerprint`, what the daemon
   looks at). Its git directory is asked of git once per stack. The event
   is a snapshot, `{"type":"worktrees","worktrees":{"<stack id>":"<version>"}}`,
   as every hub event starts with one, rather than one event per change: a
   dashboard that connects again misses nothing. Looking at a local folder
   is not asking anyone for state (ADR-0073's line between timers that ask
   and ones that look here).
2. **`GET /hub/stacks/{id}/worktrees`**: the worktrees but the main one, in
   the shape of a lane not started (`active: false`), named as the daemon
   names them (`lane_name` of the folder; a second one of the same name
   gets `-2` there and not here), with `main_stack_file` and `not_in_pool`
   for the stack file it runs with.
3. **`POST /hub/stacks/{id}/lanes/start`** (`profile` too): `path` must be a worktree git
   lists for the stack's repository, not the main tree; `on` a node,
   `<pool>:<node>` or `pool`, else `--on local`. The hub runs `devstack up`
   in the worktree, its output in `up-<id>-lane.log`; a start that fails
   before the daemon runs is the stack's `start_error`, as a stack's start.
   `202` once started.
4. **The dashboard.** The sidebar lists every stack's worktrees, of a
   stopped one from the hub, and reads a stack's lanes again when its
   version in `worktrees` changes. A stopped stack's lane opens the same
   start page as a lane of a running stack (machines, the coordinator's
   order, profiles; `LaneStart`), and its starts go through the hub route
   above; once the daemon runs, the lane's own start and views take over.
   The main working tree of a stopped stack shows that page too, instead of
   its views and their tabs: *Start here* starts the stack, *Start in the
   pool* is off with why (the main tree runs on this machine; a worktree's
   lane runs in the pool). The banner above says only that the stack does
   not run, and has no start of its own then. The branch picker finds a
   stopped stack's worktrees too, and a lane it makes for a stopped stack
   opens on its start page. A stopped stack's trees show no running dot,
   whatever was seen last.

## Alternatives considered

- **Following each running daemon's stream from the hub**: lanes would
  arrive as they are (started, stopped), but a connection per stack, and a
  stopped stack has no daemon to follow.
- **A file watcher** (`notify`): fewer wakeups, another dependency and its
  platform differences, for a folder of a handful of entries; the daemon
  looks the same way already.

## Consequences

- The hub reads the worktree folders of every registered stack once a
  second; a stack whose repository is gone is skipped.
- A lane of a stopped stack starts without the main tree's services; its
  stack shows as running then (its daemon runs).

## Enforced by

`crates/devstack-hub/src/worktrees.rs`, `crates/devstack-hub/src/lib.rs`
(the routes and `/hub/events`), `crates/devstack-daemon/src/lanes.rs`
(`fingerprint`), `ui/src/components/Sidebar.vue`, `ui/src/stores/stack.ts`
(`offlineLanes`, `offlineLane`, `startOfflineLane`),
`ui/src/components/LaneStart.vue`, `ui/src/components/BranchPicker.vue`.
