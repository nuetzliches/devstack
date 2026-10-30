# ADR-0082: The ports of a machine: every stack says what it holds, a stack can run shifted

- **Status:** Accepted (card in the pull request); built in steps
- **Date:** 2026-10-30
- **Deciders:** project owner
- **Related:** [ADR-0010](0010-promotion-mechanism.md) (the proxy on a service's fixed port), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the registry of stacks), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a lane's ports moved by its offset), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) and [ADR-0056](0056-a-lane-on-a-node-relays-its-udp-ports-through-a-tunnel-per-session.md) (a lane on a node, its ports forwarded here), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (a lane's containers on moved host ports), [ADR-0043](0043-a-dev-servers-fixed-port-is-devstacks.md) (a dev server's front), [ADR-0048](0048-a-containers-main-port-can-move-with-the-main-slot.md) (`main_ports`), [ADR-0057](0057-a-port-a-container-announces-moves-inside-it-too.md) (announced ports), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md), [ADR-0080](0080-the-main-tree-answers-as-a-lane-shared-is-deprecated.md) (the main tree answers as the lane `main`), [ADR-0081](0081-a-stack-has-a-page-every-lane-the-same-view.md) (the stack's page), [#387](https://github.com/nuetzliches/devstack/issues/387) (several stacks on one machine)

## Context

A developer with several projects, each its own repository and stack
file, often has them declare the same ports (5000, 4200, 5432). Started
side by side, the second stack fails at a service's start with "port N is
already in use". Nothing in devstack compares ports across stacks: the
registry of stacks holds no ports, a lane's number avoids only its own
stack's ports, and what a stack holds is spread over the proxies, the dev
servers' fronts, the held ports of `main_ports` containers, the
containers' published ports and the forwards of lanes on nodes. Lanes
solve the same problem inside one repository with an offset; nothing does
for a whole stack.

## Decision

The owner approved the points in the conversation on **2026-10-30**:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A daemon reports every port its stack holds on this machine, per lane (`GET /v1/ports`): port, protocol, lane, what holds it (service, container, proxy, forward) and where it leads. The hub joins every stack's (`GET /hub/ports`) and marks overlaps. | Port knowledge scattered over proxies, fronts, held ports, containers and forwards, with no view across stacks. | Low: two read-only routes. |
| 2 | concept | The stack page shows the port world as a graph: fixed ports (main slot) → the lane serving them → its services and containers with instance and published ports; lanes on nodes with their forwards; overlaps with other stacks in red. | Ports read off a table per service, and collisions found at a failed start. | UI only. |
| 3 | concept | A stack can run on this machine with a port shift: everything a lane offset moves (services, Compose host ports, announced ports, `urls`, the lanes too) moves by it; Compose project, volumes and data stay. | Editing every repository's ports by hand, on every machine, against the other machines' choices. | Medium: one more state of a main tree that `promote --lane`, `main_ports` and the dashboard know. |
| 4 | contract | The shift is machine-local, in the stack's registry entry, never in the stack file. | A committed `port_offset`: the same on every machine, so it only moves the collision. | Low: an entry field with a default. |
| 5 | contract | Every daemon writes the ports it claims into its registry entry (main slot after shift plus its lanes' ranges); every allocation (shift, lane number) avoids all other registered stacks' claims, stopped ones too. | Today's OS probe, which misses a stopped stack and lets one stack's lane range land on another's main ports. | Low to medium: the registry grows a field older readers ignore. |
| 6 | term | Nobody moves silently: `up` colliding with a running stack starts nothing, names it and offers `--shift auto` / `--shift <n>` (and a stack-page button); a stopped stack's collision only warns. Whoever was first keeps its ports. | Shifting the second stack by itself, which breaks bookmarks and hard-coded ports without a word. | Low: an automatic shift later is additive. |
| 7 | concept | A chosen shift sticks to the stack on this machine until `--shift none`. | A shift recomputed per start: another port every day. | Low. |

Decided by the agent alone (not asked):

- The shift is a multiple of 1000. `auto` takes the lowest one whose
  ports, lanes 1–3 included, are free of every claim.
- `devstack doctor` lists overlaps between stacks.
- A shifted stack's lanes take their offset from the shifted ports.
- Ports hard-coded outside placeholders do not follow; this is documented.
- Registry entries whose folder or stack file is gone are no claim.
- The stack file does not change.

## Built

Points 1 (0.33.0) and 2 are built:

1. **`GET /v1/ports`** answers `{ports: [PortEntry]}`, one entry per port
   and protocol a tree holds or uses: `port`, `proto` (`tcp`, `udp`),
   `lane` (`main` or the lane's name), `kind`, `holder` (a service, or a
   container as `<group>.<service>`), `via`, `target`, `target_lane`,
   `node`, `listening`. The kinds: `fixed` (a service's port; the main
   slot's in `main`, moved by the offset in a lane), `instance` (an
   instance behind the proxy or a front), `published` (a container's host
   port), `held` (a `main_ports` container's fixed port), `node` (a port of
   a lane on a node forwarded or relayed here). `via`: `proxy` (the HTTP
   proxy), `forward` (a front, a held port, the main slot's forward to a
   lane, a lane on a node), `direct` (the process itself), `docker`. While
   a lane serves the main slot, the main tree's fixed and held ports name
   it in `target_lane`. `listening` is what devstack knows, not a probe.
   The main tree's daemon includes its lanes' entries (a lane here from its
   supervisor, a lane on a node from its forwards here, each port named by
   the main tree's service or container it moves);
   `/v1/lanes/<lane>/v1/ports` is that lane's own. A container's protocol
   comes from `compose ps` (`Protocol`).
2. **`GET /hub/ports`** asks every running stack's daemon on request (no
   polling) and answers `{stacks: [{id, name, running, ports}], overlaps:
   [{port, proto, stacks: [{id, name, lane, holder, kind}]}]}`. A stack
   whose daemon does not run, or does not answer (an older devstack), has
   what its stack file declares for the main tree, `kind: "declared"`: its
   services' `port`, its Compose groups' `announced_ports` and the ports on
   this machine their `urls` name. Other published ports of the Compose
   files are not read: that takes `docker compose config`. A stopped
   stack whose stack file is gone is left out. An overlap is one port and
   protocol in more than one stack; an instance port does not count (the
   OS hands it out, so two running processes never share one).
3. **`devstack doctor`** warns of each overlap with its port and the
   stacks, lanes and holders, from the hub when it runs, else read the same
   way without it (each running daemon, each stopped stack's file). It
   does not start the hub for that.
4. The dashboard's mock answers both routes, with one overlap; the types
   are in `ui/src/api/types.ts`.
5. **The stack page's Ports card** (point 2) reads the daemon's `/v1/ports`
   and `/hub/ports` when the page opens, when the stack's lanes or the main
   slot change, and every 10 s while it is shown; a stack that does not run
   shows what its stack file declares, from the hub. It draws one band per
   lane, `main` first: on the left the machine's fixed ports of the stack
   (the main tree's fixed, held and published ports, in the band of the
   lane they lead to, `target_lane` or `main`) and a lane on a node's
   forwarded ports with the node's name; in the middle the lane, the one
   serving the main ports marked; on the right its holders (services,
   `<group>.<service>` containers) with their own ports. SVG connectors
   join port, lane and holder; hovering or focusing one highlights its
   path. A port another stack uses too is red, its title names that
   stack, lane and holder, the header counts them and a list below the
   graph repeats them. Instance ports are hidden behind a toggle; lanes
   without ports collapse into one line; what does not listen is dashed.
   Below 560 px of card width a grouped list (lane, holder, ports) says
   the same. The mock's instance ports are stable, and its shop has a lane
   on a node (`review-cart` on `build-server`) from the start.

What follows: the shift with the claims in the registry, `up --shift`, the
stack page's button and the start check (points 3 to 7).

## Consequences

- One place says which ports a stack holds and why; the graph, the claims
  and `doctor` read it instead of each collecting it again.
- An overlap with a stopped stack is visible before either starts, as far
  as its stack file declares its ports.
- A port only a Compose file publishes, without an announcement or a
  `urls` entry, is not seen in a stopped stack.
