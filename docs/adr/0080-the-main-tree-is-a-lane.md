# ADR-0080: The main working tree is a lane; the daemon, the main slot and what runs once per machine are the stack's

- **Status:** Accepted (card in the pull request, points 1 to 7 approved 2026-10-30); built in steps, see "Built"
- **Date:** 2026-10-30
- **Deciders:** project owner
- **Related:** [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md) (workers in the active slot), [ADR-0010](0010-promotion-mechanism.md) (the proxy holds the fixed ports), [ADR-0023](0023-stack-file-compatibility.md) (a renamed key keeps an alias), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a lane is a supervisor in the main tree's daemon), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (each lane has its own infrastructure; `shared`), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (a lane's measured need), [ADR-0068](0068-a-stop-takes-the-containers-too.md) (a stop takes the containers), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (a stopped stack's lane starts without the main tree), [#387](https://github.com/nuetzliches/devstack/issues/387) (several stacks on one machine)

## Context

ADR-0024 made every other worktree a lane of the main working tree's
daemon. The main tree itself stayed apart: the daemon is its process, it
owns the artifact store and the fixed ports, its `down` ends the daemon,
`[compose.<group>] shared` services are its containers that lanes use, a
`lanes = false` service runs in it only, and it alone runs background
workers by default. About 130 places in the daemon tell the main tree from
a lane.

Three things the project wants next run into that difference: a port shift
for a whole stack when several stacks on one machine declare the same ports
(#387), a page per stack above its lanes, and starting the main tree on
another machine as a lane can be. The owner's principle: the main tree's
data is not special. With a correct stack file every lane holds everything
of its own, and nothing of one lane talks to another; worktrees and lanes
are decoupled completely.

## Decision

The owner approved the review card in the conversation on **2026-10-30**,
all seven points:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | The daemon belongs to the stack (artifact store, main slot, registry entry, shared services) and runs without any running lane. | The daemon as the main tree's process. | Medium: its lifecycle and the restart (ADR-0052). |
| concept | The main working tree is the lane `main`, equal to and decoupled from every other lane; no lane reaches another. | The main tree as a special case. | High once built upon. |
| contract | Nothing breaks for `main`: offset 0, Compose project `<project>`, its volumes and `/v1/...` stay; `/v1/lanes/main/...` is added. | Renaming it with a data migration. | Low. |
| concept | `shared` belongs to the stack: started with the first lane that needs it, ended with the last, run as its own Compose project `<project>-shared`. | Lanes using the main tree's containers. | Medium. |
| contract | `singleton = true` replaces `lanes = false` (an alias after ADR-0023): exactly once per machine, in the lane that holds the main slot, moving with `promote --lane`. | Main tree only. | Low to medium. |
| term | `devstack down` stops one's own lane, in the main tree too; `--stack` and the dashboard's "Stop stack" stop everything. ADR-0068 point 3's exception goes. | `down` in the main tree ending everything. | Low, but visible. |
| term | The main slot (the fixed ports) belongs to the stack and points at one lane or none; that lane runs the background workers, others only with `--workers` or a profile. It is where #387's shift will live. | Fixed ports and workers as the main tree's. | Low. |

Decided by the agent alone (not asked): the memory check compares a new
lane's measured need (ADR-0049) with the machine's free memory instead of
with what the main tree takes; the daemon, not the main tree, prunes the
artifact store; a lane without a stack file of its own keeps falling back
to the main tree's for now.

## Built

- **0.33.0:** `singleton = true` is the key; `lanes = false` still works,
  means the same and is reported by `devstack check` as deprecated
  (`compat::DEPRECATED`). Its behaviour is unchanged so far: main tree only.
  `/v1/lanes/main/v1/...` answers as the main tree's `/v1/...` (no lane is
  ever named `main`: a worktree folder of that name becomes `lane-main`).

Still to build, each in a pull request of its own: the daemon prunes the
store and the memory check uses measured need; `<project>-shared`; the
main slot as one lane or none, with the singletons and the workers
following it; `down` and `down --stack`.

## Consequences

- A client (the dashboard, MCP, the CLI) can address every tree as
  `/v1/lanes/<name>/v1/...`, `main` included, and stop telling the main
  tree apart (the dashboard's part is ADR-0081).
- Stack files keep loading unchanged; `lanes = false` stays readable until
  a `version = 2` (ADR-0023).
