# ADR-0024: A lane is a supervisor in the main working tree's daemon

- **Status:** Accepted, amended 2026-10-15 twice (the lanes are the worktrees; git owns them, lanes run from a copy)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** plan §3.5, [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md), [ADR-0010](0010-promotion-mechanism.md), [ADR-0012](0012-shared-infrastructure-between-lanes.md), [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md) to [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (remote lanes, proposed)

## Context

Plan §3.5 defines a lane as a source root (the main working tree or another
`git worktree`) with its own snapshot, artifacts and candidates, of which one
owns the main slot. Worktrees are how several people and, more and more, AI
agents work on one repository at the same time; each needs a running stack
for its branch without stopping the others.

Before lanes, a worktree with a copy of the stack file started a daemon of its
own: the same fixed ports as the main tree (conflict), the same Compose project
names (the containers of one tree served the other), workers twice.

## Decision

1. **One daemon per repository, one lane per worktree.** Every linked
   worktree of a repository with a stack file is a **lane of the main tree's
   daemon**: `git worktree list` is the source of truth, so a worktree added
   is a lane at once, not started, and one removed is no lane any more. The
   daemon follows `<git common dir>/worktrees` (read only) and tells its
   clients within a second (a `lanes` event). **Worktrees are git's:**
   devstack never creates or removes one; git, an IDE or an agent tool does.
   The CLI and the MCP server in a worktree detect this and talk to that
   daemon; `devstack up` there starts the lane, `devstack lane stop` stops it.
   Nothing has to be configured. The dashboard shows a lane like a stack of its
   own, under its repository and named by its branch; the lane's name (from
   the directory) is what the API, `--lane` and `{lane}` use.
2. **A lane is a supervisor of its own** inside that daemon: source the
   worktree, the worktree's own `devstack.toml` when its branch has one (else
   the main tree's), state in `.devstack/lane-<name>/` (snapshot, logs).
   **Nothing of a lane runs in its worktree.** Builds run in its snapshot
   (ADR-0002), and so do its dev servers, tools, jobs without a build and
   hot-reload mode; `{repo}` is the snapshot for them. The snapshot follows
   the worktree 150 ms after a change, so a dev server's own watcher (hot
   module replacement) sees it. What the snapshot lacks because it is ignored
   (`node_modules`) a service's `prepare` installs there. So a tool may remove
   a worktree at any time, also on Windows, where a process inside it would
   hold it open; the lane stops with it. The
   **artifact store is the main tree's**: a build id is a hash of its inputs,
   so a worktree on the same sources starts from the main tree's artifacts
   without building, and two trees never build the same thing twice. Only the
   main tree prunes the store, keeping what any lane runs. Its API is the main one under `/v1/lanes/<name>/v1/...`, so every
   tool (CLI, MCP, dashboard) works on a lane unchanged (ADR-0007).
3. **Ports move by a fixed step per lane.** Every service port is moved by
   `[lanes] port_step` (default 100) times the lane's number; the first number
   whose whole range is free is taken. The moved ports are the lane's own
   proxies (ADR-0010), so candidates and promotion work inside a lane exactly as
   in the main tree.
4. *Replaced by [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (2026-10-16): every lane runs its own Compose
   projects; sharing is opt-in per service.* As first decided: infrastructure
   is shared and owned by the main tree; a lane reads its published ports,
   separates its data by `{lane}` / `{lane_suffix}` and runs without
   workers (ADR-0012 interim rule).
5. **`promote --lane <name>` moves the main slot.** The fixed port of every
   proxied service forwards to the lane's port, and the workers move: the old
   owner restarts its worker services without them, the new owner with them.
   `--lane main` gives everything back. Dev servers and services with
   `proxy = false` that run in the lane get a plain TCP forward on their fixed
   port instead (an HTTP proxy in front of a TLS dev server would break it;
   bytes pass unchanged, WebSockets included); one that does not run in the
   lane keeps running in the main tree. A service with `lanes = false` never
   runs in a lane (something that may run once per machine): the main tree's
   instance serves every lane and keeps its workers wherever the main slot is.
   While a lane holds the main slot, the
   main tree builds no candidates for the moved services.
6. **Who owns the main slot is runtime state**, one value shared by every
   supervisor of the daemon. A daemon restart starts clean (ADR-0016) with the
   main tree owning it; the lanes start only with `devstack up` in their
   worktree. `.devstack/lanes.json` remembers each lane's number, so its ports
   stay the same.
7. **A worktree removed takes its lane along**: the lane stops and its state
   (snapshot, logs) is deleted; its builds stay in the shared store until
   pruned.

*Amended 2026-10-15, before any other user.* The first version made a worktree
a lane only on `devstack up` in it or `devstack lane add <path>`, and kept
lanes in `lanes.json` after their worktree was gone. A worktree that exists is
what a developer or an agent has open; the dashboard and `lane list` show
exactly those, and `lane add` accepted any folder, not only a worktree of this
repository. Now git decides, and `lane add <name>` creates the worktree.

*Amended again 2026-10-15, before any other user.* `lane add` and
`lane remove` wrapped `git worktree add` and `remove` with a path convention of
their own, a second way to the same result that agent tools and IDEs do not
know; they are gone, devstack only follows git. A new worktree showed up only
when a client asked, not in an open dashboard: the daemon now follows the
worktrees directory. Dev servers and hot-reload mode ran from the worktree
itself, so on Windows `git worktree remove` failed while they ran: they run
from the snapshot now, which is also what a remote lane does
([ADR-0025](0025-a-lane-can-run-on-another-node.md)), so a lane runs the same
way wherever it is. The cost is the time the snapshot takes to follow
(150 ms plus the copy of the changed files) before hot module replacement
sees a change.

## Alternatives considered

- **One daemon per worktree, coordinated.** Each daemon would need to agree
  with the others on ports, infrastructure ownership and the main slot: a
  distributed protocol on one machine, for state one process can hold.
- **One Compose project per lane** (ADR-0012 option A). Correct but heavy: a
  second SQL Server or search engine per agent exceeds a laptop's memory. Kept
  for remote lanes ([ADR-0025](0025-a-lane-can-run-on-another-node.md)), where the memory is a node's.
- **An artifact store per lane.** Simpler, but every new worktree would
  build everything cold (minutes for a .NET service) although the main tree
  has the same build already. Publishing is atomic per id (a temporary
  directory renamed into place), so concurrent publishes are safe; pruning is
  kept to the main tree.
- **Dev servers in the worktree, with a hint to stop the lane before
  removing it.** No delay before hot module replacement, but the tool that
  removes worktrees (git, an agent tool) fails, and devstack stands in the way
  of a workflow it does not own.
- **`node_modules` linked into the snapshot** (junction or symlink) instead of
  `prepare`. Nothing to install, but the dev server would run executables from
  the worktree's `node_modules` again, which is the lock this avoids, and a
  worktree made by git has none.
- **An HTTP proxy for dev servers as well.** Would break TLS dev servers
  (`ng serve --ssl`); a TCP forward needs no knowledge of the protocol.

## Consequences

- Agents in worktrees get a full stack per branch with one `devstack up`, on
  predictable ports (`+100`, `+200`, ...), and their tools act on their lane.
- A dev server whose command ignores `{port}` cannot run in a lane (its fixed
  port is taken by the main tree); the start fails with a port-in-use error.
- Disk: lanes add only the builds of their own changes to the shared store;
  the main tree keeps the newest five per service plus every pinned one.
- A stack file change on a lane's branch applies to that lane only.

## Enforced by

`crates/devstack-daemon/src/lanes.rs` (lanes from `git worktree list`,
`Lanes::follow`, port ranges, the main-slot hand-over), `Supervisor::new_lane`,
`run_root`, `copy_ready`, `copy_changed` and the shared `main_owner` in
`crates/devstack-daemon/src/supervisor/`, the lane branch of `watch.rs`,
`prepare` in `crates/devstack-config`, `/v1/lanes` in `api.rs`, worktree
detection in `crates/devstack/src/client.rs`, the stack switcher in
`ui/src/components/StackSwitcher.vue`, the integration test
`a_worktree_runs_as_a_lane` (git removes the worktree of a running lane; a new
worktree is found without a request).
