# ADR-0113: The share switch runs the node agent: in the hub on Linux, in WSL2 on Windows

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-18
- **Deciders:** project owner
- **Related:** [ADR-0031](0031-a-coordinator-places-lanes.md) (point 3, amended 2026-10-21: the switch removed), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (admins grant nodes), [ADR-0033](0033-a-node-decides-what-runs-on-it.md), [ADR-0011](0011-platform-windows-native-first.md), [ADR-0112](0112-a-node-gives-a-lane-the-gpus-it-asked-for.md) (GPUs), [#171](https://github.com/nuetzliches/devstack/issues/171), [#168](https://github.com/nuetzliches/devstack/issues/168), [#170](https://github.com/nuetzliches/devstack/issues/170)

## Context

The share switch was removed on 2026-10-21 (ADR-0031, amended): it reported
capacity but never started a node agent, so no lane ever ran on a machine
that only switched it on. #171 asked for a switch that makes the machine a
node, and #168 for a way to run the agent on Windows, where lanes (Linux
containers) cannot run natively; #168 leaned to Docker-in-Docker in Docker
Desktop.

A spike on 2026-11-18 (branch `spike/168-wsl-gpu-node`, its result in
#168) ran the released Linux agent in a WSL2 distribution with Docker Engine
of its own, `networkingMode=mirrored` and a Hyper-V firewall rule, as a
member of its own: another member's lane started on it and its container saw
the machine's NVIDIA GPU. It also showed what made that hard by hand: two
members for one machine, a terminal window that must stay open, `join` run
on the wrong side, and limits above what WSL may take.

## Decision

The owner approved the review card in the conversation on **2026-11-18**,
points 1 to 5:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | The share switch makes the machine a node: on Linux the agent runs in the hub (as #171 proposes), on Windows the hub starts it in a WSL2 distribution (`wsl.exe -d <distro> -u root -- devstack node agent …`), starts it again when it ends, and ends it when switched off. | Docker-in-Docker (#168): a privileged container and an image of its own; an agent in a terminal window that must stay open | Medium: the hub supervises a process, and the switch depends on it |
| 2 | concept | The agent in WSL serves the machine's membership (the hub's), read from the Windows side: one member per machine; while it runs, the hub does not report (#170). | Two members per machine as in the spike: two grants, the Windows side's taken back by hand | Medium: the agent reads its membership from elsewhere |
| 3 | trust | The hub does not set the distribution up: it checks it (the spike's `check.sh`, in the hub) and shows what is missing, as commands and as a prompt for a coding agent; what needs elevation or ends Docker Desktop (the Hyper-V rule, `wsl --shutdown`) only the user does. | A hub that installs packages, rewrites `.wslconfig` and ends Docker Desktop | Low |
| 4 | contract | The caps come from the switch's settings and stay below what `.wslconfig` gives WSL (`memory=`); a node at a workplace is never `--dedicated`. | Caps above the VM's memory: the node offers more than it has | Low |
| 5 | contract | Off means no new lanes; running lanes go on until their owners stop them, and the switch shows how many still run. | Ending running lanes at once (would need an amendment to ADR-0033) | Low |

As built:

1. **The switch** (`crates/devstack-hub/src/nodehost.rs`, `Switch`): per
   pool `{enabled, max_cpus?, max_memory?, distro?, port?}` in
   `node-host.json` beside the membership. A file of its own: the
   `share.json` of the removed switch may still say `enabled` and must not
   start an agent after an update. `PUT /hub/pool/share` saves it (an
   approved member only, impossible values refused) and acts at once;
   `GET` carries it (`switch`) and the agent (`host`).
2. **The supervisor** (`nodehost::round`, every 5 s per pool): from the
   switch, what runs and the other members' lanes here (`decide`) it
   starts, restarts (other caps, distribution or port), drains or stops
   the agent. An agent that ends within 30 s of its start is `failed` and
   started again after a minute; its last 40 lines of output are kept. An
   agent started by hand for the membership (its `node/lock` held) wins:
   `foreign`, the switch starts none. Whether an admin granted the
   machine is read from the member list (`granted`).
3. **Linux:** `devstack_node::node::serve` on a thread of the hub, with the
   machine's membership and `0.0.0.0:<port>`; its API is the address this
   machine reaches the coordinator from (`local_address`, a UDP socket
   connected, nothing sent).
4. **Windows:** the membership's files are copied into
   `/var/lib/devstack-node/<pool>` in the distribution (root only, folders
   left out: the Windows side's state), and the agent runs as `wsl.exe -d
   <distro> -u root -- sh -c 'exec devstack node agent --membership … '`,
   with `~/.local/bin` and WSL's GPU tools on `PATH`. A new hub first ends
   one an earlier hub left running there (`pkill` on its `--membership`).
   The hub knows the agent it runs and reports nothing meanwhile (the
   membership's lock is in WSL, not seen on Windows).
5. **The checks** (`nodehost::wsl::judge`): the distribution exists, its
   networking mode is `mirrored`, `docker` is an engine of its own (not
   Docker Desktop), devstack there is this machine's version: required.
   An NVIDIA GPU with the container toolkit and the Hyper-V firewall rule
   `devstack-node` (read with `Get-NetFirewallHyperVRule`): shown, not
   required. Each missing one has the command for it; together they make
   the prompt (`host.prompt`), copied in the dashboard.
6. **Off with guests** (point 5): the agent is started again with
   `--no-new-lanes`, which reports `sharing: false` and offers nothing;
   once no other member's lane runs here, it ends. The node's report
   carries `sharing` from its settings now (it said `true` always).
7. **The agent's CLI:** `devstack node agent --membership <dir>` (point 2)
   and `--no-new-lanes` (point 5).
8. **The dashboard** (`ShareSwitch.vue`): the switch in the sidebar and in
   **Pool → This machine**, with the distribution (Windows), the caps, the
   state, the checks with a copy button per command, the setup prompt and
   the agent's last lines.

Decided while building, not on the card:

- The prompt is copied from the switch, not handed to an agent the hub
  starts (the prompt registry of ADR-0046 is keyed by a stack or a folder;
  this one is about the machine).
- Point 4 holds without a check of its own: the agent in WSL measures what
  is free inside the VM, so half of it is below `.wslconfig`'s `memory=`
  by construction; the caps can only lower it.
- An agent the switch runs takes no update an admin orders (ADR-0039,
  `on_update: None`): it runs the hub's devstack, and the hub is updated
  with the machine.
- The switch is per pool, like the membership; the default port is 4261,
  so two pools switched on need different ports.
- Not on macOS yet (ADR-0054): the switch says so and stays off.

## Alternatives considered

- **Docker-in-Docker in Docker Desktop** (#168's direction): a hard cap and
  a separate image cache, but a privileged container, an image to build and
  pin, and Desktop's VM shared with the user's own containers. WSL2 with an
  engine of its own needs none of that and was proven by the spike.
- **A member of its own for the distribution** (the spike's way): works
  today, but every machine shows twice and needs two grants.
- **The hub setting the distribution up:** quicker for the user, but it
  would install packages as root, rewrite `.wslconfig` and end Docker
  Desktop without being asked.

## Consequences

- A Windows machine with an NVIDIA GPU can offer it to the pool from the
  dashboard, once its distribution is set up.
- The membership's key is copied into the distribution, readable by its
  root only; it is the same user's machine.
- The hub on Linux runs other members' lanes in its own process: a hub
  restart restarts the agent (the lanes keep running, their leases
  persist).
- Verified by unit tests (the step from switch to agent, the agent's
  arguments, the distribution's checks and prompt, the WSL list, an old
  `share.json`), an API test (no switch without a membership), the
  dashboard's tests; not yet on a Windows machine with WSL, which the next
  run on the spike's machine does.

## Enforced by

`crates/devstack-hub/src/nodehost.rs`, `crates/devstack-hub/src/share.rs`
(no report while the switch's agent runs, `PUT /hub/pool/share`),
`crates/devstack-node/src/node.rs` (`sharing` from the settings),
`crates/devstack/src/poolcmd.rs` (`--membership`, `--no-new-lanes`),
`ui/src/components/ShareSwitch.vue`.

## Addendum 2026-11-19: a setup dialog before the switch

Switched on, the switch went green at once and then said "Not ready yet",
while what was missing stood only on the page "This machine", and the
admin's grant ("Runs the pool's lanes") was a second switch elsewhere
without a visible link to it. The owner approved the card on 2026-11-19:

1. Switching it on opens a setup dialog instead of saving `enabled`: what
   sharing does (and that an admin has to grant the machine), the WSL
   distribution, the caps and the checks with the command for each missing
   piece, then the start.
2. The hub checks without switching: `POST /hub/pool/share/check` runs what
   a start runs (the switch's own problems, on Windows the distribution's
   checks, the address toward the coordinator) and saves nothing. Before,
   the checks ran only once the switch was on.
3. The switch and the sidebar tile are green only while the agent runs and
   the machine is granted; amber, with a link into the dialog, while the
   agent is blocked or failed; the accent colour while it starts or waits
   for an admin.
4. The last step shows the admin's grant as its own state; an admin can
   grant the machine there.

A check's `fix` is now the command alone, to be copied; `fix_note` says
where it runs. Decided while building: the page keeps its caps form for
later changes; an admin's grant in the dialog is a checkbox, on by default;
turning the switch off needs no dialog. Enforced by
`crates/devstack-hub/src/share.rs` (`check`), `nodehost.rs` (`preflight`),
`ui/src/components/ShareSetupDialog.vue`, `ShareChecks.vue`.
