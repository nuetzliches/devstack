# ADR-0041: A profile says how a lane (or the main tree) runs for a purpose, chosen when it starts

- **Status:** Accepted (2026-10-19); point 1 extended by [ADR-0058](0058-a-profile-can-leave-out-containers-and-services.md) (Proposed: `skip`)
- **Date:** 2026-10-19
- **Deciders:** project owner
- **Related:** [#115](https://github.com/nuetzliches/devstack/issues/115), [#103](https://github.com/nuetzliches/devstack/issues/103), [ADR-0017](0017-stack-file-is-toml.md) (its "no profiles in version 1" meant environments; amended), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (point 6: workers in lanes), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md)

## Context

A lane runs the stack the way the stack file describes it for daily work:
dev servers that watch, no background workers, the data the seed jobs
produce once. That suits working on a branch. It is the wrong shape for the
other common reason to start a lane: **running a given commit for an
acceptance test**, such as an end-to-end suite before a release or a bug
reproduced on an older commit.

A downstream team assembled such a run by hand and lost to lane artefacts
(#115, #103):

- the SPA ran as a dev server and went away mid-run;
- without workers, every test waiting for a scheduled job or a webhook
  consumer failed;
- the test data needed a manual seed command;
- the suite needed several environment variables for the lane's shifted
  ports.

The same commit run through a separate hand-written Compose setup was green,
while the lane run had nine failures, all of them lane artefacts. That
separate script is what devstack should make unnecessary.

The pieces exist already:

- `kind = "static"` builds a frontend into an artifact and serves it, with
  no watch;
- `--workers` turns workers on in a lane (ADR-0030 point 6);
- seed jobs and `[commands]`;
- placeholders for a lane's addresses (`{service.web.url}`,
  `{compose.app.db.5432}`).

What is missing is one name that says "run it like this" and is chosen at
start.

## Decision

1. **A profile is a named overlay in the stack file**, `[profiles.<name>]`.
   It changes how the stack runs, never what it is. A profile can set:
   - `workers = true | false`: background workers in a lane, as `--workers`
     does;
   - `data = "keep" | "fresh"`: `fresh` removes the tree's own Compose
     volumes before its infrastructure starts (here; on a node, its kept
     slot, ADR-0038), so the seed jobs fill a new database;
   - `env`: variables for every service and command of the tree, with the
     usual placeholders (`{service.web.url}`, `{compose.app.db.5432}`,
     `{lane}`);
   - `autostart`: the services that start, instead of each service's own
     `autostart`;
   - `[profiles.<name>.services.<service>]`: per service, the fields of a
     service that say how it runs: `kind` (for instance `static` instead of
     `devserver`), `build`, `output`, `run`, `env`, `health`. A service the
     profile does not name runs as the stack file says.
   - `command`: a `[commands]` entry that is the profile's purpose (the
     suite). `devstack up --profile acceptance --run` runs it once the tree
     is ready and returns its result.
2. **It is chosen when a tree starts**:
   - `devstack up --profile <name>` (in a worktree or the main tree);
   - `devstack lane start <lane> --profile <name>`;
   - the MCP `up` and `lane_start` with `profile`;
   - the dashboard's start of a lane offers the profiles.

   The tree runs with it until it stops. `lane list`, `status` and the
   dashboard show it. A start without `--profile` runs without one. A
   profile is not part of a lane's identity: the same worktree can run the
   acceptance profile now and the plain stack later.
3. **A profile is checked like the rest of the stack file.**
   `devstack check` validates each profile's merged services by the rules
   for services (a `static` needs `build`, `output` and `port`, and so on).
   A profile naming a service or a field that does not exist is an error,
   not a silent no-op.
4. **The candidate pipeline holds within a profile.** A `static` service
   is built into an artifact (ADR-0002) and a change still goes through a
   candidate (ADR-0003). An acceptance run of a commit has no changes, so
   it serves the build it started with. ADR-0008 holds: in a lane with
   `workers = true` that does not serve the main ports, the workers run as
   `--workers` runs them (ADR-0030 point 6).
5. **A lane with a profile can serve the main ports** (`promote --lane`)
   like any lane. `status` says that it does, with its profile, since a
   static or data-fresh tree on the main ports is a deliberate choice.
6. **Format.** `[profiles]` is new and optional, so existing files are
   unchanged (ADR-0023). A file that uses it raises `[stack] requires` to
   the release that reads it.

## Alternatives considered

- **Flags only** (`--workers --fresh --static web --env …`). Each run is
  assembled again by whoever runs it, and an agent has to know the recipe.
  The point is a name in the repository that says how.
- **A second stack file for acceptance.** It duplicates everything that is
  the same, and the two drift apart.
- **Run images a CI built** (`variant = "image"` in #115). That is a
  different decision: a native service would become a container, with
  ADR-0004 (Compose owns containers) and ADR-0014 to consider. It is left
  for later. An image the team wants to run can be a Compose service of a
  group today, and a profile can choose which services autostart.

## Consequences

- The stack file gets a second level of service settings. `devstack env`,
  `devstack check` and the dashboard must show the effective settings of
  the profile a tree runs with, or the overlay becomes a source of
  surprises.
- `data = "fresh"` throws away data on every start with that profile, and
  says so at the start. The main tree with `fresh` is refused unless
  `--yes`, since its data is usually the one that matters.
- A profile's `env` sees the tree's own addresses. A suite started by
  `--run` therefore needs no hand-set ports.

## Enforced by

*Paths as built, noted 2026-10-19:* `crates/devstack-config/src/model.rs`
(`Profile`, `Stack::with_profile`) and `validate.rs` (each profile checked
as the stack it makes), `tests/profiles.rs`; the daemon applies
`DEVSTACK_PROFILE` at start (`crates/devstack-daemon/src/lib.rs`), a lane's
from its own stack file (`lanes.rs`: `open`, `check_profile`), `data =
"fresh"` in `Supervisor::fresh_data` and, on a node, in `LaneRun::start`;
the profile's env reaches `exec` steps (`commands.rs`); `--profile`,
`--run`, `--yes` in `crates/devstack/src/client.rs`, MCP `up`/`lane_start`;
the dashboard's lane start and stack header.

Where it will live:

- `crates/devstack-config`: `[profiles]`, the merge (`Stack::with_profile`),
  the checks; `docs/stack-file.md`; the compat files in `tests/compat/`.
- `crates/devstack-daemon`: the profile per supervisor (main tree and
  lanes), `data = "fresh"` before `infra_up`, the `env` of every service
  and command, `POST /v1/up` and `POST /v1/lanes` with `profile`; the
  lane's view and `status`.
- `crates/devstack-node`: `fresh` for a lane on a node: its slot's volumes
  go before its start (ADR-0038).
- `crates/devstack`: `up --profile [--run]`, `lane start --profile`, MCP
  `up`/`lane_start` with `profile`; the skill's recipe.
- `ui/`: the profile at a lane's start, and shown with a running tree.

## What this ADR does not say

- That a service can run from a container image a CI built. That is a
  later decision (see Alternatives).
- That a profile can change the Compose files of a group. It chooses which
  of a group's services start; what a group is stays in its Compose files.
