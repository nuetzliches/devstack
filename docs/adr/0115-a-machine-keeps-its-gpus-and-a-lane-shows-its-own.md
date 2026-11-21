# ADR-0115: A machine keeps the GPUs its owner wants, and a lane shows its own

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-20
- **Deciders:** project owner
- **Related:** [ADR-0112](0112-a-node-gives-a-lane-the-gpus-it-asked-for.md) (a node gives a lane its GPUs; "no policy key for GPUs" left for when a node owner asks), [ADR-0113](0113-the-share-switch-runs-the-node-agent.md) (the share switch and its caps), [ADR-0114](0114-a-start-in-the-pool-can-ask-for-gpus.md) (the machine cards' GPU line), [ADR-0031](0031-a-coordinator-places-lanes.md) (point 3: what a machine shares)

## Context

The share switch capped CPU and memory; a shared machine offered every GPU
no lane held. Its owner could not keep a GPU for themselves, and a lane of
the pool got a GPU the owner was using right then (a training of their
own), since the node checked only the GPU's total memory. And a lane's
usage showed CPU, memory and disk, never the GPU it ran on; the machines
table showed a GPU at most as a badge.

## Decision

The owner approved the review card in the conversation on **2026-11-20**,
points 1 to 4:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | term | The machines table's "Shares" column says the GPUs too: `14.2 CPU · 6.7 GB · 1 GPU free` or `held`. | A "GPU" badge with a tooltip | Low: display only |
| 2 | contract | A lane holding GPUs shows a fourth tile "GPU" on its lane card and usage page: their load and memory, with their history; the values come from its node. | No GPU in a lane's usage | Medium: a field in the usage API |
| 3 | trust | The share switch takes `max_gpus`: the most GPUs it offers the pool; `0` keeps them for its owner; not set, as before, all. | Every GPU of a shared machine free for the pool | Low: an optional key; old files load as before |
| 4 | trust | A GPU whose memory processes outside the pool's lanes use for more than a quarter is not offered; it shows as "in use on the machine". | A pool lane given a GPU its owner is using | Low; the threshold is an assumption |

As built:

1. **The offer** (`share::offered_gpus`, `node.rs` `held_gpus`): a GPU a
   lease holds names its lane; of the others, one with more than a quarter
   of its memory used (`IN_USE`) is `busy` and not offered, and of the rest
   only as many are offered as the cap leaves (`gpu_room`: `max_gpus` less
   the GPUs the leases hold, counted from the leases, not from what
   `nvidia-smi` lists now), the ones with the most memory first. The
   coordinator, which counts offered GPUs only, places by it without a
   change. The cap is a number of GPUs: the admission (`assign_gpus`) gives
   any free GPU that is not busy and has the memory asked for, as long as
   the room allows (amended after the reviews before 0.52.0: the first
   build offered and gave the first GPUs in the tool's order, so a lane
   that needed the second, larger one was refused, and a GPU missing from
   the tool's list did not count against the cap).
2. **The cap:** `share::Settings::max_gpus`, the agent's `--max-gpus`, the
   switch's `max_gpus` in `node-host.json` (`PUT /hub/pool/share`). The hub
   passes `--max-gpus` only when it is set and the agent takes new lanes,
   so an agent of an earlier release in a WSL distribution starts as
   before, also one that only lets running lanes finish. The dashboard's
   switch and setup dialog show a "GPU cap" where the machine has a GPU,
   and say when what is typed is no whole number instead of saving it as
   no cap.
3. **A lane's GPUs** (`laneinfra::refresh`, `node.rs` `lane-infra` thread):
   once a round, where a lease holds GPUs, the node asks `nvidia-smi` (at
   most 5 s: it hangs at times, and the same thread reports every lane's
   containers) and
   writes the lease's GPUs (name, memory, used, load; no id) into the lane's
   file beside its containers' load. The lane's daemon (`usage.rs`
   `sample_external`) keeps them as a series of its own and reports them as
   the lane's `gpu` (`count`, `name`, `memory`, `memory_used`, `load`, the
   load summed in percent of one GPU, as CPU); a round without them keeps
   the last for a few rounds. Their memory does not count into the lane's
   expected memory need.
4. **The dashboard:** the lane card's GPU tile (load, sparkline, memory
   used of all), the machines table's `· 1 GPU free`, and "in use on the
   machine" beside such a GPU on a machine's page and in its title.

Decided while building, not on the card:

- No GPU tile for a lane on this machine: it holds no GPU of its own, and
  the machine's GPU shows under "This machine".
- No GPU column per process in the usage table: in a container the process
  ids do not match those `nvidia-smi` names.
- No `share_percent` for GPUs: a GPU goes to one lane whole.
- A GPU a lane holds is never `busy`, whatever it uses; a held GPU above the
  cap stays held (lowering the cap takes nothing from a running lane).
- The node measures a lane's GPUs only while one of its leases holds a GPU.

## Alternatives considered

- **A policy key for GPUs** (the coordinator's or the node's
  `policy.json`): the policy is the admin's; keeping a GPU is the owner's
  choice, like the CPU and memory caps on the switch.
- **`nvidia-smi` inside the lane container:** it sees only the lane's GPUs,
  but needs the toolkit's utilities in every lane image; the node measures
  them already.
- **Offering a GPU in use when it has room** (by free memory): a lane would
  share the GPU with the owner's process, which ADR-0112 ruled out between
  lanes.

## Consequences

- An owner shares CPU and memory and keeps the GPU with a GPU cap of 0, or
  shares it and gets it back from the pool while they use it themselves.
- On a desktop the display uses some GPU memory; a GPU with less than four
  times that is never offered. The threshold can move if a node owner asks.
- Verified by unit tests (offer, cap and busy, assignment, the lane file,
  the daemon's series, the hub's agent arguments, the dashboard's texts) and
  in the mock dashboard; with a real GPU on the WSL node of #168.
