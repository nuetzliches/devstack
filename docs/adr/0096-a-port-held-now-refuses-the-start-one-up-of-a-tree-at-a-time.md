# ADR-0096: A port held now refuses the start; one `up` of a tree at a time

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-05
- **Deciders:** project owner
- **Related:** [ADR-0082](0082-the-ports-of-a-machine.md) (the ports of a machine; point 6 amended here), [ADR-0068](0068-a-stop-takes-the-containers-too.md) (a stop takes the containers), [ADR-0095](0095-the-daemon-ends-with-the-last-tree.md) (the daemon ends with the last tree; an `up` under way keeps it), [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) (point 2: the main tree's profile comes with its start), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A stack was added on a machine where another stack, a downstream one, was
registered and stopped. Its daemon had ended with the machine's session;
its containers had not stayed down: they carry `restart: unless-stopped`,
and when the hub started Docker Desktop, Docker started them again. The
new stack's `up` compared its ports with the registry's claims (ADR-0082
point 5), found the stopped stack's, and only warned (point 6). Compose
failed two minutes later: `Bind for 127.0.0.1:4317 failed: port is already
allocated`. The pool's telemetry only had `failed (exit code: 1): …`; the
cause was in the daemon's local log. The other stack's start was then
refused for the new one's ports, though the new one held none.

While the first `up` still pulled an image, the dashboard's start sent a
second `POST /v1/up`. Both ran `docker compose up` on one project and took
each other's container names (`Conflict. The container name "/…" is
already in use`).

## Decision

The owner approved the points in the conversation on **2026-11-05**, all
four:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | term | `up` checks the ports it is about to take on the machine too: a port in use now refuses the start, as a running stack's claim does. This amends ADR-0082 point 6: a stopped stack's claim only warns while its ports are free. | A warning while the port is taken, and a Compose failure minutes later, said only in the local log | Low: one more check before the start |
| 2 | concept | A stack whose daemon does not run but whose containers do holds ports, for the port check, `doctor` and `/hub/ports`. The holder is found by its Compose project and named ("the containers of the stack `docs` hold 4317, 5432"); a port no stack's container holds is "another program". | "Stopped" by the daemon alone, while Docker has long started the stack's containers again | Low to medium: one more state of a stack the hub and the dashboard show |
| 3 | term | The refusal offers what frees the ports: `devstack down` in that other stack (its containers go, its data stays), or the shift of the starting stack as before. | The shift as the only way out, though forgotten containers are usually all that is in the way | Low: a text and a button |
| 4 | concept | A tree has one `up` at a time: a second `up` with the same profile joins the one under way (`202`), one with another profile is refused with `409`, naming the one under way. | Two `docker compose up` on one project, taking each other's container names | Low |

Decided by the agent alone (not asked):

- The ports the starting stack's own Compose project publishes do not
  count as held: they are left from its own last run, and Compose takes
  them over.
- Whether a port is in use is asked the way `--shift auto` asks it
  already (`port_in_use`); who holds it, of `docker ps` and the
  `com.docker.compose.project` label, only when a port is in use.
- A container belongs to the registered stack whose Compose project it
  carries: a group's `project` for the main tree, `<project>-<lane>` for a
  lane; of several, the longest project that fits.
- The port of a service `up` does not start (`autostart = false`) is not
  checked as held: it is taken only when the service starts, and that
  start fails on it as before.
- The lanes' port allocation is unchanged: it avoids the other stacks'
  claims and what is in use already.
- devstack does not touch the restart policy of anyone's Compose files.
- Built on ADR-0095's branch, which changes the same start path.
- Not fixed here, found on the way: after a Compose failure ("up
  stopped") `devstack up` keeps following the start until its own timeout
  instead of ending with the error.

## Built

1. **The port check before `up`** (`crates/devstack/src/shift.rs`): of the
   ports the main tree will take (after its shift), those in use now are
   matched to the running Compose containers. One of another registered
   stack's (running or not) refuses the start, as does one no stack's
   container holds; one of the stack's own is its own. A stopped stack's
   claim on a free port still only warns. The refusal names who holds
   what, says `devstack down` in the stack whose containers hold ports,
   how to find another program (`Get-NetTCPConnection -LocalPort <p>`,
   `lsof -nP -iTCP:<p> -sTCP:LISTEN`), and offers the shift as before
   (`devstack up --shift auto`, which the dashboard looks for). A stack
   that never ran has no claims yet: the host ports its Compose files
   publish are read with `docker compose config` before its first start,
   so its first `up` is checked too (found by the hand test on Windows:
   without it, nothing was compared).
2. **The containers of a stack** (`devstack_daemon::leftover`):
   `docker ps` with project, service and ports
   (`Docker::published_by_projects`, parsed by `parse_project_ports`),
   matched to the registered stacks' Compose projects.
3. **`GET /hub/ports`**: a stopped stack has, besides what its stack file
   declares, what its running containers publish: those ports as
   `kind: "published"`, `listening: true` (the declared entry of the same
   port goes), and `containers: true` on the stack. Docker is asked once
   per request, only while a stack is stopped. `devstack doctor` reads the
   same way without the hub, so its overlaps name those ports too.
4. **`devstack down` without a daemon** removes the main tree's containers
   that Docker has (`docker compose -p <project> down`, volumes kept;
   with `--all` the lanes' too) and says which projects; with
   `--keep-infra`, or nothing found, it says "no daemon is running" as
   before. `POST /hub/stacks/{id}/down` does the same for a stack whose
   daemon does not run: `200` with `{"removed": [projects]}`, `503` when
   there was nothing to remove.
5. **The dashboard**: a stopped stack's page whose ports another stopped
   stack's containers hold names them ("The containers of docs hold 4317,
   though its daemon does not run") and has *Remove containers of docs*;
   a stopped stack whose own containers run says so, with the ports, and
   has *Remove containers*. Both ask first. The mock shows it with
   `?mock=strays`.
6. **One `up` of a tree at a time** (`Supervisor::begin_up`): `POST /v1/up`
   takes the tree's start before it applies a profile. One under way:
   `202` with `{"joined": true}` when no other profile is asked, `409`
   when one is. A lane's cold restart that finds one under way joins it.

## Consequences

- A stack whose containers Docker brought back no longer blocks a start
  minutes into it: the start refuses at once, says whose containers hold
  which port, and the dashboard removes them with one button.
- `devstack down` is the way to stop a stack's containers whatever its
  daemon does; before, it did nothing without one.
- Not done here: the telemetry still cuts a Compose error's text to `…`,
  so the pool sees `failed (exit code: 1): …` without the cause.
