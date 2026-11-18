# ADR-0112: A node gives a lane the GPUs it asked for, and no other lane

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-18
- **Deciders:** project owner
- **Related:** [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (what a lane's Compose files may ask of a node), [ADR-0036](0036-the-pool-shows-its-capacity-over-time.md) (point 4: GPUs reported before they are placed), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (how a lane asks for a GPU), [ADR-0065](0065-the-store-is-warmed-for-a-ref-before-a-lane-needs-it.md) (warm leases), [#168](https://github.com/nuetzliches/devstack/issues/168) (a Windows machine's GPU as a node)

## Context

A lane asks for GPUs with `[lanes.need] gpus` (ADR-0049), the coordinator
places it only on a node that offers enough (ADR-0036), and the node
admitted it when the machine had that many. Nothing more happened: the
lane's own container got no GPU, so a native service (a Python worker, a
model server) never saw one. A Compose service could reserve GPUs, any
lane's, as many as it liked (`count: all`), since ADR-0033's check let
every `nvidia` device reservation through. And the node offered every GPU
in every report, so two lanes could be admitted onto the same GPU and run
each other out of memory.

## Decision

The owner approved the review card in the conversation on **2026-11-18**,
points 1 to 5:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | A node gives a lane's container exactly as many GPUs as its lease asks for (`--gpus device=<ids>`), never `all`, and none to a lane without a GPU need. | No GPU for native services (as before); `--gpus all` for every lane | Low: one argument of the lane container |
| 2 | concept | A node assigns each GPU to one lease until it ends; a held GPU is not offered, and the coordinator and the admission count free GPUs only. | Several lanes on one GPU: memory overbooked, two trainings run out of it | Medium: the assignment is node state that placement and the report depend on |
| 3 | trust | A lane's Compose service may reserve GPUs only within its lease's assignment: the node rewrites the reservation to the assigned ids and refuses GPU reservations of a lane without a GPU need. | Every lane reserving every GPU through Compose (as before) | Low in code; a stack that took a GPU on a node through Compose without `[lanes.need] gpus` is refused there, and the message names the key |
| 4 | contract | The node keeps the assignment in its state: after a restart of the agent a running lane keeps its GPUs and none is given twice. | An assignment in memory only, given twice after a restart | Low |
| 5 | term | A node's report and the pool area show each GPU as free, held by lane X, or not offered. | A report that says only "offered", not who holds a GPU | Low: one field in the report, one word in the dashboard |

As built:

1. **The assignment** (`share::assign_gpus`, `node.rs` `admit`): the
   lease's `need.gpus`, unless it is warm, gets the first GPUs in
   `nvidia-smi`'s order that no lease of the node holds and that have the
   memory it asks for; fewer than it asks for refuses the lease (`409`,
   "has n free GPU(s) with … or more"). Read before the leases are
   locked, decided with them locked, so two admissions cannot take the
   same GPU. The ids are `nvidia-smi`'s `uuid`, stored in the lease's
   `gpus` in `leases.json` and freed with the lease.
2. **The lane container** (`lanerun.rs`, `gpus_arg`): `docker run --gpus
   "device=<id>,<id>"`, the quotes keeping the commas inside Docker's one
   CSV field.
3. **Compose** (`confine::pin_gpus`, after `check`, before `render`): in
   each service the lane starts, every `gpus` entry and every `deploy`
   device reservation that is a GPU (the `nvidia` driver, or no driver and
   the `gpu` capability, as `check` tells them) gets `device_ids`: `count:
   n` the first n of the lane's GPUs, `-1`, `all` or no count all of them,
   `device_ids` only ids among them; its `count` goes (Compose takes one
   of the two). A lane holding none, more than it holds, or an id not its
   own is refused with the service's name.
4. **The report** (`node.rs`, `held_gpus`): a GPU a lease holds is
   `offered: false` with `lane: "<repo>/<lane>"`, every other one
   `offered: true`. A coordinator of an earlier release already counts
   only offered GPUs, so it places correctly too. The GPU's id stays on
   the node (`#[serde(skip)]`): the report carries name, memory, load,
   `offered` and `lane`.
5. **The dashboard** (`PoolMachine.vue`): "free", "held by <repo>/<lane>"
   or "not offered" beside each GPU.

Decided while building, not on the card:

- No policy key for GPUs (at most n per lane, or none on this node): the
  policy says already who may start lanes on a node. Left for when a
  node owner asks.
- First fit, in the vendor tool's order; no packing.
- A warm lease holds no GPU: it runs `prepare` only.
- A lane the node keeps after its lease (ADR-0038) holds no GPU; its next
  lease is assigned afresh.
- A lease an agent of an earlier release admitted has no assignment: its
  lane gets no GPU until its next lease.

## Alternatives considered

- **`--gpus all` for a lane that asks for any:** simplest, but a lane
  sees every GPU of the machine, and two lanes share them.
- **Sharing GPUs by memory** (a lane asks for 8 GiB of a 24 GiB GPU):
  `nvidia-smi` cannot enforce it; one lane runs the others out of memory.
- **Leaving Compose reservations as they are:** a lane without a GPU need
  takes every GPU through its Compose files, and the assignment means
  nothing.

## Consequences

- A lane in the pool that needs a GPU says so in `[lanes.need] gpus`;
  without it, a Compose service that asks for one is refused on a node
  (on this machine nothing changes).
- A node needs `nvidia-smi` and the NVIDIA Container Toolkit for its
  engine; without the tool it reports no GPU, as before.
- Verified by unit tests (assignment, report, the container's argument,
  the Compose rewrite and its refusals); with a real GPU by the spike of
  #168 (a Windows machine's GPU through WSL2), not yet run.
