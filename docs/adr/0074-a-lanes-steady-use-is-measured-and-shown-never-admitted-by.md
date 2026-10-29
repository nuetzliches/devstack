# ADR-0074: A lane's steady use is measured and shown, never admitted by

- **Status:** Accepted (2026-10-29)
- **Date:** 2026-10-29
- **Deciders:** project owner
- **Related:** [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (the need, the footprint), [ADR-0066](0066-a-measured-cpu-need-is-capped-at-half-of-what-its-node-gives-lanes.md) (the CPU cap), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A lane's need (ADR-0049) is the most each part took in its repository's
last five runs, its build included. The dashboard said "The lane takes
about 8.7 CPU and 7.2 GB" beside a node whose six running lanes used about
4 CPU and 14 GB together. It read as what the lane costs while it runs, and
was what it takes while it builds.

Admission already treats it so: a node offers what it shares less what its
lanes *use* (measured), and holds a lease's need only while it settles
(three minutes, `SETTLING`). The need decides whether a start fits the room
free now, and sets the lane's memory limit. What was missing was what a lane
takes once it is up: a node measured it every 15 s and kept only the peak.
The node's history (ADR-0036) and the telemetry (ADR-0037) hold the node's
use, not a lane's.

## Decision

The owner approved the review card in the conversation on **2026-10-29**,
all four points:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | A node also measures a run's steady use: the 95th percentile of its CPU (the minute's average, capped as in ADR-0066) and of its memory, from the readings after the lane is up. | Only the peak, which reads as a permanent cost. | Low: one more field per run, dropped again. |
| contract | The footprint keeps `steady` beside `peak` per run (the most of the last five, as for the peak), and the placement view carries it as an additive field an older node omits. | Deriving it from the node's history, which cannot tell lanes apart. | Medium: one field in `footprints.json` and the API, additive (ADR-0023 not touched). |
| concept | Admission, the hold while a lease settles and the container's memory limit stay on the peak; `steady` decides nothing. | Admitting by a percentile, which ADR-0049 rejected and which lets builds started together collide or run out of memory. | None: no decision changes. |
| term | The dashboard and the CLI say both: "starts with up to 8.7 CPU and 7.2 GB, its build included, then runs on about 0.8 CPU and 2.1 GB". | One number that mixes the build and the run. | Low: text only. |

Decided while building, not on the card:

- **Up** is when the node logs `node_lane_started`. The minute's CPU window
  starts again there, so a minute that holds the build's end does not
  count. A lane the node agent finds running when it starts again counts as
  up.
- Readings are counted in steps (a tenth of a core, 64 MiB), each rounded
  up, so a run of weeks keeps a few hundred numbers. Steady use is reported
  once both parts have a reading.
- A run reported again replaces its steady use rather than raising it: the
  later percentile covers more of the run. A report without one keeps the
  last.
- The lease offer does not carry it; the placement view and
  `devstack pool placement` (and the MCP `placement`) do.

## Consequences

- Without a run that measured it (stored before, or a node of an earlier
  release), the dashboard shows the peak alone, as before.
- A run's steady use is at most its peak (it is taken from the same
  readings, rounded up to a step). Hot reloads count: a lane rebuilding more
  than 5 % of the time shows a higher steady use.

## Code

`crates/devstack-node/src/runs.rs` (`Steps`, `Runs::up`),
`footprint.rs` (`Run::steady`, `Footprints::steady`), `coordinator.rs`
(`Placement::steady`), `node.rs` (up at `node_lane_started`),
`crates/devstack/src/client.rs` (`placement`), `ui/src/components/LaneStart.vue`.
