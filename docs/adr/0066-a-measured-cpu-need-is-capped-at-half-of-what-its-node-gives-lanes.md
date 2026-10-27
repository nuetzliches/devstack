# ADR-0066: A measured CPU need is capped at half of what its node gives lanes

- **Status:** Accepted (2026-10-27)
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

1. **A node caps the CPU it measures for a run** at half of the most it
   offers its lanes: the part of its cores it shares (its policy's
   `share_percent`, else half, ADR-0031 point 3), at most its `max_cpus`.
   That is what an idle node offers; a lane that took more took what was
   free. Half says "a lot", and leaves an idle node room for it beside
   another such lane. (Half of `max_cpus` or of the cores alone would not:
   a node without `max_cpus` offers half its cores, so a lane measured at
   half its cores would need all of an idle node's offer.)
2. The cap applies to each round's reading before the minute's average
   (ADR-0049 point 2), so the run's peak, its report and the footprint stay
   within it. Memory, disk and GPUs are measured as before.
3. Admission and placement are unchanged: CPU still counts, against an
   offer (ADR-0026, ADR-0031), and the need still has the stack file's
   `[lanes.need]` as its floor (ADR-0049 point 5): a stack that does need
   more than half a node declares it.
4. **CPU counts from capped runs only.** A node with the cap marks its runs
   so (`capped`); the coordinator keeps the mark with each run in
   `footprints.json`. The CPU of a run without it, stored before or
   reported by an older node, does not count; its memory and disk do. A run
   reported uncapped and then capped (its node was updated while it ran)
   takes the capped CPU. Until a capped run is there, the floor stands for
   the CPU: `[lanes.need]`, else the least (ADR-0033 point 4).

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
- **Remove the stored footprints by hand once.** It needs the coordinator
  stopped (it writes the file back from memory) and someone who knows the
  file; point 4 does it on its own.

## Consequences

- A repository's measured CPU need on a node stays within half of what that
  node offers its lanes; on the node above, sharing 80 %, 9.6 CPU. Its lanes
  fit an idle node beside a second one.
- A build may still take more than its need while it runs; the offer is
  measured (ADR-0031), so the next placement sees it.
- A repository locked out by an uncapped footprint starts again once the
  coordinator runs this version: its stored CPU no longer counts, nothing
  is edited by hand.
- Until its first capped run, a repository's lanes are placed with the
  floor's CPU, possibly fewer cores than they take. CPU only slows a lane
  down, and the offer is measured, so the next placement sees the load.
- The requester's estimate (ADR-0049 point 4.2) is not capped: it applies
  only before a stack's first run in the pool.

## Enforced by

- `crates/devstack-node/src/runs.rs` (`Runs::load` takes the cap, a run
  is reported `capped`), `crates/devstack-node/src/share.rs`
  (`Settings::most_cpus`, the most a node offers),
  `crates/devstack-node/src/node.rs` (the agent's round passes half of it), `crates/devstack-node/src/footprint.rs` (`Run::capped`,
  the entry's `capped`, `Footprints::get` counts CPU from capped runs).
