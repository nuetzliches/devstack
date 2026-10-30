# ADR-0080: The main working tree answers as the lane `main`; `singleton` replaces `lanes = false`; `shared` is deprecated

- **Status:** Accepted (2026-10-30; card in the pull request)
- **Date:** 2026-10-30
- **Deciders:** project owner
- **Related:** [ADR-0023](0023-stack-file-compatibility.md) (a renamed key keeps an alias; a break needs `version = 2`), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a lane is a supervisor in the main tree's daemon; point 5, `lanes = false`), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (each lane has its own infrastructure; point 3, `shared`), [ADR-0058](0058-a-profile-can-leave-out-containers-and-services.md) (a profile's `skip`), [ADR-0068](0068-a-stop-takes-the-containers-too.md) (point 3: what a main tree's stop keeps for the lanes), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md), [ADR-0078](0078-the-hub-follows-every-stacks-worktrees.md) (a lane starts without the main tree's services), [#387](https://github.com/nuetzliches/devstack/issues/387) (several stacks on one machine)

## Context

Three things the project wants next look as if they need the main working
tree to be a lane like any other: a port shift for a whole stack when
several stacks on one machine declare the same ports (#387), a page per
stack above its lanes with one view for every tree, and fewer ties between
a lane and the main tree. The owner's principle: the main tree's data is
not special; with a correct stack file every lane holds everything of its
own, and nothing of one lane talks to another.

A first card made the main tree the lane `main` inside the daemon as well
(the daemon, the main slot and what runs once per machine the stack's;
`down` stopping one's own lane; workers and singletons following the main
slot). It was approved and then, the same day, weighed again and withdrawn
before anything of it beyond this ADR's points was built:

- None of the goals needs it. #387's shift belongs to the fixed ports and
  the machine's registry; a port graph needs a model of the ports; one view
  for every tree needs one API for every tree. Only starting the main
  checkout itself on another machine does, and a worktree promoted to the
  main ports does that today.
- The pain behind it was a lane on a node needing a local start of the main
  tree. Since 0.31.0 (ADR-0078) a lane's start brings up the daemon only;
  the main tree's services stay stopped. The daemon itself has to run here:
  it syncs a node's lane, renews its lease and forwards its ports.
- The main tree as a lane with offset 0 keeps its exceptions (its ports are
  the main slot's, it runs in place, its Compose project has no suffix):
  about 130 places in the daemon's most tested code would change and the
  difference would remain under another name. A main tree that is truly a
  lane needs its own offset and every fixed port held by a forward, a
  breaking change of its own, decided when it is needed.

What does tie a lane to the main tree is `[compose.<group>] shared`: a lane
here uses the main tree's containers. Only native services reach them (a
lane's containers do not find them by name, so one downstream stack lost
the telemetry of its lanes' containers without a word); a lane on a node
runs its own, so the same lane behaves differently by place; every lane
sends into the same telemetry; a lane on another branch gets the main
tree's definition; and `down` in the main tree keeps containers for the
lanes (ADR-0068 point 3). What it saves is memory, which a profile's `skip`
saves too, visibly.

## Decision

The owner approved the points in the conversation on **2026-10-30**:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| contract | `/v1/lanes/main/v1/...` is the main working tree's `/v1/...`, so a client addresses every tree as a lane. No lane is ever named `main` (a worktree folder of that name is the lane `lane-main`). | Clients telling the main tree apart by path. | Low: an added route. |
| contract | `singleton = true` names a service that runs once per machine, in the main working tree only. `lanes = false` stays an alias that `devstack check` reports as deprecated. | A name that says where a service does not run instead of what it is. | Low. |
| contract | `[compose.<group>] shared` is deprecated: it works as before, `devstack check` and `devstack doctor` report it, and it goes with `version = 2`. Every lane then starts all of its containers; a profile's `skip` leaves out what a lane can do without. | Building it out (a stack-owned `<project>-shared`, network aliases into every lane): more coupling for a saving a profile gives too. | Low until `version = 2`. |

Decided by the agent alone (not asked): the deprecation messages name the
replacement; the reference keeps `shared` documented, marked deprecated,
with the reasons.

## Consequences

- Stack files load and behave as before; `devstack check` names the two
  deprecated keys with what replaces them.
- The dashboard can show every tree, the main one included, through one API
  (its part: one view for every tree, a page per stack, ADR-0081).
- When `version = 2` comes, `shared` goes and with it ADR-0068 point 3's
  exception and ADR-0030 point 3.
- The daemon stays the main working tree's process. Making the main tree a
  lane inside the daemon is a decision of its own, taken when starting the
  main checkout on another machine is needed.
