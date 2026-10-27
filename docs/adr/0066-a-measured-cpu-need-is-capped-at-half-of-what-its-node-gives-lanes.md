# ADR-0066: A measured CPU need is capped at half of what its node gives lanes

- **Status:** Proposed
- **Date:** 2026-10-27
- **Deciders:** project owner
- **Related:** [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (amends points 2 and 3), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md), [ADR-0031](0031-a-coordinator-places-lanes.md), [ADR-0033](0033-a-node-decides-what-runs-on-it.md)

## Context

ADR-0049 makes a lane's need the most each part took in its repository's
last five runs, CPU as its peak averaged over a minute. Memory measured that
way is a need: a lane that gets less is killed. CPU is not: a build
(`dotnet build`, an Angular build, `cargo`) starts as many workers as there
are cores and takes what is free. Its peak measures how idle the node was,
not what the lane needs; with less it runs slower, not wrong.

Seen in a pool (a downstream .NET + Angular stack, one 24-core node,
`--max-cpus 20`): over two days the measured need of the repository's lanes
rose 3.8, 4.2, 5.5, 7.1, 7.9, 10.4, then 18.5 CPU, each run building on a
node with more free than the one before. The node offers 20 CPU less what
its lanes use; four idle lanes use 2 to 3, so it offers about 17. Every
start since is refused with 18.5 CPU needed, memory far from short. No lane
of the repository runs, so no run replaces the 18.5: the maximum of five
never moves. The repository is locked out of the pool until someone edits
the coordinator's `footprints.json`.

## Decision

1. **A node caps the CPU it measures for a run** at half of what it gives
   its lanes in all: its `max_cpus` when it has one, else its cores. A lane
   that took more took what was free; half says "a lot", and leaves an idle
   node room for it beside another such lane.
2. The cap applies to each round's reading before the minute's average
   (ADR-0049 point 2), so the run's peak, its report and the footprint stay
   within it. Memory, disk and GPUs are measured as before.
3. Admission and placement are unchanged: CPU still counts, against an
   offer (ADR-0026, ADR-0031), and the need still has the stack file's
   `[lanes.need]` as its floor (ADR-0049 point 5): a stack that does need
   more than half a node declares it.

## Alternatives considered

- **Admit by memory and disk only, CPU a placement score.** Closer to what
  CPU is, but it changes admission in ADR-0026 and ADR-0031, and ADR-0033's
  per-lane CPU limit would need another source. Larger than the failure.
- **Cap at the coordinator, per node it places on.** It would also bring
  down a footprint stored before this ADR, but the coordinator knows a
  node's cores (`total`), not its `max_cpus`, so a cap there can still lie
  above every offer.
- **Forget a footprint no node can take.** It ends the lock-out after the
  fact, the ratchet towards it stays.

## Consequences

- A repository's measured CPU need on a node stays within half of what that
  node gives lanes; on the node above, 10 CPU. Its lanes fit an idle node
  beside a second one.
- A build may still take more than its need while it runs; the offer is
  measured (ADR-0031), so the next placement sees it.
- A footprint stored before a node with this cap reports is not lowered by
  it (a run's peak is raised, never lowered, ADR-0049 point 3); it ages out
  after five runs. A repository locked out already needs its entry removed
  from the coordinator's `footprints.json` once.
- The requester's estimate (ADR-0049 point 4.2) is not capped: it applies
  only before a stack's first run in the pool.

## Paths

- `crates/devstack-node/src/runs.rs` (`Runs::load` takes the cap),
  `crates/devstack-node/src/node.rs` (the agent's round passes it).
