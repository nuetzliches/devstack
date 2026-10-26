# ADR-0064: Every Compose call is told the lane it runs for

- **Status:** Proposed
- **Date:** 2026-10-26
- **Deciders:** project owner
- **Related:** [#280](https://github.com/nuetzliches/devstack/issues/280), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md), [ADR-0057](0057-a-port-a-container-announces-moves-inside-it-too.md)

## Context

The main tree and every lane serve their pages on `localhost`, and a
browser keeps one cookie per name whatever the port: a login in a lane's
page signs the main tree's out. For a native service the stack-file
reference recommends a cookie name with `{lane_suffix}` in its `env`. A
Compose service has no such way: `[compose.<group>]` has no `env`, and the
stack file's placeholders never reach the Compose files. What a downstream
stack did instead, `app_refresh_${COMPOSE_PROJECT_NAME}`, changes the main
tree's value too (the files give another name than without devstack),
hangs on the group's `project` where `devstack check` cannot see it, and is
built from the project instead of the lane.

ADR-0057 already sets variables (`DEVSTACK_PORT_*`) in the environment of
every `docker compose` call for a group and of every `exec` step, with the
files reading them with a default (ADR-0015). Native processes get
`DEVSTACK_LANE` already (ADR-0014 step 4).

## Decision

1. **Every `docker compose` call devstack makes for a group** (the model it
   reads, `up`, `ps`, `logs`, `stop`, `down`, a lane's overlay check), and
   every `exec` step of a command, gets `DEVSTACK_LANE` (the lane's name,
   `main` in the main tree) and `DEVSTACK_LANE_SUFFIX` (empty in the main
   tree, `_<lane>` in a lane): the values `{lane}` and `{lane_suffix}` have
   for a native service in the same tree. Like `DEVSTACK_PORT_*` they win
   over an env file's value. A call for a lane's `shared` service is the
   main tree's project and gets the main tree's values.
2. **A lane on a node** gets its name as its requester calls it: the node's
   agent sets both for its Compose calls (the model checked in a container,
   `create`, `start`, `down`), and the lane's daemon there, which runs as a
   main tree, sets the same for its `exec` steps (from
   `DEVSTACK_REMOTE_LANE`).
3. **The files read them with a default** (`${DEVSTACK_LANE_SUFFIX:-}`,
   `${DEVSTACK_LANE:-main}`), so they give the main tree's value without
   devstack (ADR-0015): `REFRESH_COOKIE: app_refresh${DEVSTACK_LANE_SUFFIX:-}`
   is `app_refresh` in the main tree and outside devstack, `app_refresh_feat-x`
   in lane `feat-x`.
4. **`devstack check` warns**, from the group's Compose files and env files
   as text: `DEVSTACK_LANE` or `DEVSTACK_LANE_SUFFIX` read without a
   default, and a `DEVSTACK_LANE_*` devstack does not set (a typo, which
   would be empty in every tree). A name being set (`VAR=` in an env file, a
   YAML key) is not a read.

## Alternatives considered

- **`env` per container in the Compose group** (`[compose.app.env] scheduler
  = { REFRESH_COOKIE = "app_refresh{lane_suffix}" }`, passed as a generated
  override file). Closer to a native service's `env`, but a second place
  that defines a container's environment, beside the Compose files that do
  already, and a stack-file key (`requires`, ADR-0023) for what two
  variables give.
- **`COMPOSE_PROJECT_NAME` in the files** (what a downstream stack did).
  Needs nothing of devstack, but the main tree's value differs from the
  files' own, and it follows `project`, not the lane.
- **Only the lane's name.** Compose interpolation cannot add a leading `_`
  for a lane and nothing for the main tree, so a suffix that is empty in the
  main tree has to come from devstack.

## Consequences

- `DEVSTACK_LANE` and `DEVSTACK_LANE_SUFFIX` are devstack's: a group's env
  file that set either name itself gets devstack's value.
- No stack-file key, so no `requires`: a devstack before the release that
  brings this sets neither, and files with a default give the main tree's
  value in every tree (as before this ADR).
- `docker compose` run by hand gets neither, and so the main tree's value;
  an `exec` step has them.
- A lane on a node and the native services there differ: the daemon in the
  lane's container runs as a main tree, so `{lane}` and `{lane_suffix}` (and
  a native process's `DEVSTACK_LANE`) are the main tree's there, while
  Compose is told the lane's name. Aligning the native side is not part of
  this decision.

## Enforced by

`crates/devstack-config/src/model.rs` (`LANE_VARIABLE`,
`LANE_SUFFIX_VARIABLE`, `lane_variables`, `ComposeGroup::variables`),
`crates/devstack-config/src/validate.rs` (`lane_warnings`, from
`repo_warnings`), `crates/devstack-compose/src/lib.rs` (`Group::from_config`:
the main tree's; `Group::env`, set on every call),
`crates/devstack-daemon/src/supervisor/infra.rs` (`compose_lane`,
`group_variables`, `compose_variables`),
`crates/devstack-daemon/src/commands.rs` (`exec_env`),
`crates/devstack-node/src/lanerun.rs` (`LaneRun::variables`) and
`confine.rs` (the model checked in a container), `docs/stack-file.md`,
`docs/pool.md`, `skills/devstack-config/SKILL.md`.

## What this ADR does not say

- Anything about other per-lane values for Compose (the lane's port offset
  stays withheld, ADR-0057).
- Whether `{lane}` and `{lane_suffix}` of a native service on a node should
  be the lane's name (see Consequences).
