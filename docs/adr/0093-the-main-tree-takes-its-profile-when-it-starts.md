# ADR-0093: The main working tree takes its profile when it starts; the daemon carries none

- **Status:** Accepted (card in the pull request); built; point 6's *Start stack* gone with [ADR-0095](0095-the-daemon-ends-with-the-last-tree.md) (`main` starts on its card)
- **Date:** 2026-11-04
- **Deciders:** project owner
- **Related:** [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) (point 2: a profile is chosen when a tree starts, which the main tree now follows), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (point 2: `main`'s profile was shown disabled; superseded there), [ADR-0080](0080-the-main-tree-answers-as-a-lane-shared-is-deprecated.md) (path A: the main tree stays the daemon's home tree), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (a lane's start brings the daemon up without the main tree), [ADR-0052](0052-a-restart-carries-the-lanes-over.md) (a restart), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A lane gets its profile with its start: `POST /v1/lanes {profile}` makes
its supervisor from its stack file with that profile. The main tree got
its profile from its daemon instead: `devstack up --profile` set
`DEVSTACK_PROFILE` for the daemon it started, which applied it once to the
supervisor it keeps for the main tree. Three things followed that were
hard to see from the dashboard:

- A lane's start with a stopped daemon brings the daemon up without the
  main tree (ADR-0078), and without a profile for it: the main tree was
  then fixed to "no profile" until the daemon stopped.
- Another profile for the main tree meant stopping the daemon, and with it
  every lane on the machine.
- *Start stack* started the daemon and the main tree in one, so the state
  "daemon runs, main stopped" was normal but reached only by side ways;
  the start card of `main` showed the profile picker disabled.

ADR-0041 point 2 already said a profile is chosen when a tree starts and
holds until it stops; for the main tree this was not true.

## Decision

The owner approved the points in the conversation on **2026-11-04**, all
six:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The daemon carries no profile: `main`'s profile belongs to `main`'s start (ADR-0041 point 2), and a daemon started for a lane, by `devstack up` or by the hub has `main` stopped and without a profile until `main` starts. | The profile as a property of the process (`DEVSTACK_PROFILE`): whoever starts the daemon fixes `main`'s | Medium |
| 2 | contract | `POST /v1/up` takes an optional `{profile, yes}`: with `main` stopped it starts `main` with that profile; with `main` running under another profile it answers `409` with the reason. | Restarting the daemon for another profile, which stops every lane on the machine | Medium |
| 3 | contract | `devstack up --profile <name>` in the main tree starts a stopped `main` with that profile in the running daemon instead of refusing; it still refuses while `main` runs with another one, and names `devstack down` (which stops `main`, not the lanes). | Refusing ("`devstack down` first") whenever the daemon's profile differs | Low |
| 4 | contract | `POST /hub/stacks/{id}/up` takes `profile` and `yes` next to `shift` and passes them as `--profile` / `--yes`. | A hub start that always starts `main` without a profile | Low |
| 5 | term | `main`'s start card offers the profile picker like a lane's; a profile with `data = "fresh"` asks for a confirmation there (the CLI's `--yes`). This supersedes the disabled profile in ADR-0081 point 2. | The disabled picker with its reason | Low |
| 6 | concept | The stack page shows the daemon's state and `main`'s state separately ("daemon runs · main stopped"), and *Start stack* says that it starts the daemon and `main`. | One state for both, and a button that does not say it starts `main` | Low |

Decided while building, not on the card:

- The main tree's supervisor stays the one the daemon made (ADR-0080
  path A): its stack sits behind a lock and is replaced on a start with
  another profile, while none of its services is active. A profile changes
  how services run, never which there are, so its slots stay; what they
  show of the stack file (kind, autostart, hot reload) follows. The daemon
  keeps the stack file's stack as read, without profile or shift, to make
  the next one from.
- `profile` absent from `POST /v1/up` keeps the profile it ran with last (a
  restart within the daemon, an older client); `""` is "without one". In a
  lane's API a profile is refused (`400`): a lane's comes with
  `POST /v1/lanes`.
- `data = "fresh"` goes once per start of the main tree, not once per
  daemon: every `POST /v1/up` that names the profile while nothing runs is
  a new start.
- A node's agent still hands a lane there its profile through
  `DEVSTACK_PROFILE`: on a node the lane is the daemon's home tree.
- *Restart stack* in the dashboard starts `main` again with the profile it
  ran with, and says so in its confirmation (with fresh data: as a danger).
  *Start stack* on a stopped stack starts `main` without a profile.
- A profile's `workers` stays a lane's matter: the main tree runs its
  workers while it serves the main ports (ADR-0008), as before.

## Consequences

- The main tree and a lane are started the same way as far as a profile
  goes; the daemon's own start says nothing about either.
- A profile switch of the main tree no longer touches the lanes.
- A running main tree must still be stopped for another profile: a switch
  under running services would leave them running as the old profile said.

## Enforced by

`crates/devstack-daemon/src/supervisor/mod.rs` (`Supervisor::stack`,
`use_profile`, `set_unprofiled`; the test `profile_tests`),
`crates/devstack-daemon/src/api.rs` (`POST /v1/up`),
`crates/devstack-daemon/src/lib.rs` (the stack kept without a profile),
`crates/devstack/src/client.rs` (`ensure_daemon` without a profile,
`main_start`), `crates/devstack/src/mcp.rs` (`up`),
`crates/devstack-hub/src/lib.rs` (`profile_args`),
`crates/devstack/tests/daemon_lifecycle.rs`
(`a_profile_runs_the_stack_for_a_purpose`), `ui/src/components/LaneStart.vue`,
`ui/src/stores/stack.ts` (`startStack`, `restartStack`),
`ui/src/views/StackPage.vue` (the daemon's and `main`'s state),
`docs/dashboard-api.md`, `docs/stack-file.md`.
