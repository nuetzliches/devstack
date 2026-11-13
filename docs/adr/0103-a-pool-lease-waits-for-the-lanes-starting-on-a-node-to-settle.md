# ADR-0103: A pool lease waits for the lanes starting on a node to settle rather than be refused

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-13
- **Deciders:** project owner
- **Related:** [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (admission by the node), [ADR-0031](0031-a-coordinator-places-lanes.md) (the coordinator places), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (the need is a peak), [ADR-0051](0051-a-pool-can-grow-by-nodes-a-provisioner-makes.md) (the wait for a machine, reused here), [ADR-0075](0075-a-lanes-steady-use-is-measured-and-shown-never-admitted-by.md) (steady use is never admitted by), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md); issue #524

## Context

A node holds a lane it admitted off its offer for three minutes
(`SETTLING`), by the lane's need, which is its measured peak, build
included: the lane is not in the measured use yet. While it builds, its use
is measured as well. A second start of the same stack on the same node in
that time was refused with "no node of the pool can take the lane now",
although the node was nearly idle and took the lane three minutes later.
With several members on a pool, two starts in the same minute are common,
and each cost a retry by hand or a start here the user had not chosen.

## Decision

The owner approved the points in the conversation on **2026-11-13**:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A pool lease that no node can take now waits when some node would take it once the leases settling there have settled; it is refused (409) only when no node would, or when the wait ends. | A client that retries on every 409 (it would wait on full or offline nodes too); starts serialised per node (a node with room for two starts would make the second wait). | Medium: the wait answer stays in the pool protocol. |
| 2 | contract | A node reports what its settling leases hold and when that ends (an optional field of its report); without it, a start is refused as before. | The coordinator inferring it from when it offered the lease (not when the node admitted it; lost on a restart). | Low: one optional field. |
| 3 | concept | The wait is ADR-0051's wait for a machine with a second reason: the same `202` with a lease id, `GET /v1/leases/{id}/wait`, and the coordinator offers waiting leases in the order they came. | A second wait mechanism; a client that posts leases again (two waiters race, in no order). | Medium: shared state in the coordinator. |
| 4 | term | The wait has a fixed bound (the node's estimate plus a minute, at most five minutes) and no flag or setting; `--on local` stays the way out. | A configurable bound. | Low: a flag can come later; one taken away later would be a break. |

Decided on its own, not on the card: a warm lease does not wait; the wait is
a timed phase `settle` beside `provision` (`remote_lane_settle`, and the
phase in telemetry); `devstack pool placement` and the dashboard's
placement say beside a node how long its starting lanes hold the room; a
start's double count during its peak (measured use and held need) stays as
it is, the wait absorbs it.

As built:

1. **The node's report.** `Report.settling` (`crates/devstack-node/src/coordinator.rs`,
   `Settling { cpus, memory, ends_in }`): for the leases admitted within
   `SETTLING`, their need plus what they use now, and the seconds until the
   last of them settles (`Inner::settling` in `node.rs`). Absent with none.
   It counts their use too because the measured offer takes it off as well;
   what they use once settled is not known, so the estimate is optimistic,
   and the bound caps what that costs.
2. **The ask.** The requester asks with `POST /v1/leases?wait=provision&settle=true`.
   The card said `?wait=`; it is a parameter of its own because a
   coordinator of an earlier release compares `wait` with `provision` and
   would have read a combined value as no wait for a machine either. It
   ignores `settle`, and refuses as before.
3. **The coordinator.** When no node is eligible, `settle_target` ranks the
   nodes again with each report's `settling` added back (`placement_in`,
   `settled`); an eligible node there makes the lease wait (`settle_for`:
   lease state `settling`, a `Wait` without a machine, the limit
   `min(ends_in + 60 s, 300 s)`), answered `202` with `state: settling`
   and why ("waiting for build-box: a lane is starting there (9.0 CPU and
   4096 MiB held, about 120 s)"). `GET /v1/leases/{id}/wait` (`settle_wait`)
   offers it once a node takes it now, unless a lease that waited longer
   can be placed now (`settle_ahead`): that one goes first. A new request
   that may wait does not go before waiting leases either. Once no node
   would take it even settled, or the limit passed, the wait ends with the
   refusal it would have had, and the lease goes. Waiting leases count
   against a member's open offers, as provisioning ones do.
4. **The requester.** `wait_for_machine` in `crates/devstack-daemon/src/remote.rs`
   serves both waits; the step reads `settle: …` or `provision: …`.
5. **Tests.** `crates/devstack-node/tests/settle.rs`: a requester that does
   not wait is refused; one that waits is told why and for how long, a
   second waits behind it and is offered after it, both are refused once no
   node would take them, and a warm lease does not wait.

## Consequences

- A start waits only when the coordinator, the node and the requester run
  this release; with an older one anywhere, it is refused as before.
- A wait can end in a refusal after up to five minutes when the starting
  lanes settle on more than the estimate assumed; the refusal then names
  the bound.
- Waiting leases hold nothing on a node. Until one is offered, a requester
  of an earlier release, or one that names its node, can take the room
  first.
- ADR-0051's wait now has two reasons; its `Provisioning` answer carries
  `state: settling` for the second.
