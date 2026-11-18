# ADR-0114: A start in the pool can ask for GPUs, and the dashboard shows where they are

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-18
- **Deciders:** project owner
- **Related:** [ADR-0036](0036-the-pool-shows-its-capacity-over-time.md) (GPUs reported), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (point 5: `[lanes.need]`, a floor), [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) (a profile's `need`), [ADR-0112](0112-a-node-gives-a-lane-the-gpus-it-asked-for.md) (a node gives a lane its GPUs), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (point 3: where the coordinator places)

## Context

A lane asks for GPUs only in its stack file (`[lanes.need] gpus`, or a
profile's `need`), so a one-off GPU run (a training, a model server tried
once) meant changing the file. The coordinator already places a lane that
needs GPUs only on a node with that many free, but the dashboard's
placement preview did not pass any GPU need along, not even the stack
file's, so it ranked a node without a GPU first. And the machine cards
showed a GPU at most as a footnote.

## Decision

The owner approved the review card in the conversation on **2026-11-18**,
points 1 to 3:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | `devstack up` and `lane start` take `--gpus <n>[:<memory>]` (`--gpus 1`, `--gpus 1:12G`), the MCP tools `up`/`lane_start` a `gpus` argument; it raises the lane's need for this start, as a profile's `need` does, and never lowers what the stack file asks. | A profile in the stack file just for a GPU start | Low: one argument in CLI, API and MCP |
| 2 | contract | The coordinator places such a lane as it does today, only on a node with that many free GPUs; a start named onto a node without a fitting free GPU is refused with the reason. | — (the behaviour stays; the need has a new source) | None |
| 3 | term | Each machine card where a lane starts gets a GPU line (free, held by a lane, not offered); with a GPU need, a machine without a fitting free GPU is marked red as for CPU and memory, and the start offers a choice "GPUs: as the stack file says / 1 / 2 …". | The footnote "1 GPU (0 %)" | Low: display only |

As built:

1. **The need** (`remote::with_gpus`): the start's GPUs raise the stack
   file's floor (the more GPUs and the more memory of the two); kept per
   lane in the daemon (`Lanes::gpus`) for its cold restart and its moves,
   dropped by a start without `--gpus`. `POST /v1/lanes` takes `gpus:
   {count, memory?}`, only with `on`; `--gpus` requires `--on`. The hub's
   start of a lane whose stack is offline passes it on as `--gpus N:BYTES`.
2. **The placement preview:** `GET /v1/placement` takes `gpus` and
   `gpu_memory` and applies them as the lease's floor; the hub's
   `GET /hub/pool/placement` sends the lane's stack file's GPUs (with its
   profile) raised by `gpus` from the dashboard. An older coordinator
   ignores them, as before.
3. **The dashboard:** the GPU choice shows where a machine of the pool has
   a GPU (up to the most one machine has); it reloads the placement, whose
   reason ("offers 1 GPU …, the lane asks for 2") marks a card red, and
   goes with a start in the pool only. The machine cards' GPU line is the
   one of the same pull request (#581): free, offered of all, the memory
   over the last hour, each GPU's state in its title.

Decided while building, not on the card:

- The GPU need is not written into the lane's note for a daemon that
  restarts while the lane runs: the lane goes on with its GPUs (the node
  holds them, ADR-0112); a cold restart after that takes the stack file's
  need.
- A lane started here ignores `--gpus` (it takes no `--on`): the machine's
  GPU reaches its Compose services as before.
- The choice in the dashboard is a count; the memory per GPU stays the
  stack file's or the CLI's.

## Alternatives considered

- **Only profiles:** one per GPU count in every stack file that might want
  a GPU once.
- **A pool tag `gpu` and `--on pool:gpu`** (ADR-0033 point 2): says where,
  not how many; the syntax clashes with `<pool>:<node>` and is open.

## Consequences

- `devstack up --on pool --gpus 1` starts a lane on the node of the pool
  with a free GPU, without changing the stack file.
- The dashboard's order of the nodes now reflects a GPU need, the stack
  file's too.
- Verified by unit tests (the floor raised and never lowered, `--gpus`
  parsed and refused without `--on`, the dashboard store sends it only
  with a start in the pool) and in the mock dashboard (a node with one free
  GPU marked red for two); with a real GPU by the next run on the WSL node
  of #168.
