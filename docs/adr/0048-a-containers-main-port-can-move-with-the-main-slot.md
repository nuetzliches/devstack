# ADR-0048: A container's main port can be devstack's, and then moves with the main slot

- **Status:** Accepted (2026-10-21)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [#184](https://github.com/nuetzliches/devstack/issues/184) (part 2), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) point 5, [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md), [ADR-0043](0043-a-dev-servers-fixed-port-is-devstacks.md), [ADR-0047](0047-the-dashboard-makes-a-lane-from-a-branch.md)

## Context

`promote --lane` (ADR-0024 point 5) moves the fixed ports of `[services.*]`:
a proxied service's upstream, a dev server's forward (ADR-0043). A Compose
container publishes its port itself: the main tree runs the Compose files
unchanged, Docker binds `3306`, and devstack is not in front of it. A lane
already publishes its containers on the Compose file's host port plus its
offset (ADR-0030; a lane on a node: forwarded here on the same number,
ADR-0029), so the lane's side exists. Only the main tree's side is missing.

A stack whose processes all run in containers (Linux binaries built by a
command, a game server, a vendor image) therefore has no switch:
`promote --lane` is refused for it since #188, and ADR-0047's "a branch runs
in a lane, which then serves the main ports" does not hold there. Its other
branch runs on the lane's ports, and every client (a game client, a
desktop tool, a browser bookmark) must be pointed there and back.

A container's port behind devstack differs from a dev server's in two ways
that matter: containers outlive the daemon (`devstack down` without
`--infra` leaves them running, and they run while a daemon restarts), and a deployment
file's port may be meant for other machines (`3724:3724` binds every
interface; a game client on the LAN uses it).

What has to switch is narrow. The developer works with a few services
directly: the UI in the browser, the API a desktop tool or the browser
calls. Those have to answer on the main ports from the lane chosen. What
they use in turn may run on any port, as long as each lane's services reach
their own lane's and never another's: the goal is no crosstalk between
lanes, not every port moving.

## Decision

1. **Opt-in per Compose service.** `[compose.<group>] main_ports = ["realmd",
   "mangosd"]` names the services whose published ports are devstack's in
   the main tree. Without it nothing changes: Docker binds the ports, and
   the main slot does not move them. The list names what the developer
   reaches directly from the host (the UI, the API the browser calls), not
   what those services use: a lane's containers reach the lane's own on its
   Compose network or its moved ports (ADR-0030), whichever lane serves the
   main ports.
2. **The main tree's overlay moves those ports to an instance port.** As a
   lane's overlay does (ADR-0030), the main tree gets one for such a group:
   each TCP port of a named service is published on a port Docker assigns,
   on loopback (`127.0.0.1::<target>`); devstack reads it from `compose ps`
   as it already does.
3. **devstack holds the fixed port with a forward** (`tcpfwd::Front`,
   ADR-0043): on the address the Compose file names (none: every interface,
   as Docker would bind it; `127.0.0.1`: loopback), *hold* while the
   container starts or is recreated, *forward* to its instance port,
   *refuse* while it is stopped. A recreated container gets another
   instance port; the forward follows it.
4. **The main slot flips the forward.** `promote --lane` points it at the
   lane's port for that container: the Compose file's host port plus the
   lane's offset, published by a local lane, forwarded here for a lane on a
   node. `--lane main` points it back. Such a service counts as a main port
   for `moves_main_slot` (#184), so the stack is no longer refused.
5. **The main tree's container keeps running** while a lane has its ports.
   It has no workers to move (ADR-0008 is about `[services.*]`); only
   clients on the host reach the lane. Clients inside the main tree's
   Compose network still reach the main tree's container, and the lane's
   containers reach each other on the lane's network, as now.
6. **TCP only.** A UDP port of a named service stays Docker's in the main
   tree and does not move; the stack file check warns about it, and
   `promote --lane` says which ports stayed. devstack forwards no UDP
   anywhere yet (a lane on a node neither), which is a known limitation of
   its own ([#192](https://github.com/nuetzliches/devstack/issues/192)); once
   it does, `main_ports` moves UDP too.

## Alternatives considered

- **Always, for every published port.** It needs no setting, but it puts
  devstack in front of every database port too, which then closes when the
  daemon stops while the container runs on, the opposite of today. Opt-in
  confines that to the services whose clients have to follow a lane.
- **Recreate containers on the fixed port.** Stop the main tree's container
  and publish the lane's on the fixed host port. It costs a container
  recreation in both trees per switch, takes seconds instead of a flip, and
  for a lane on a node, there is no container here to publish anything.
- **The OS forwards (`netsh interface portproxy`, `iptables`).** It needs
  administrator rights, differs per platform, and does not hold a
  connection while a container starts.
- **Leave it, and say so (status since #188).** Stacks of containers keep no
  switch; ADR-0047's model has a hole exactly where container-only stacks
  are common.

## Consequences

- A named service's fixed port answers only while the daemon runs. A daemon
  restart closes it for a moment (a connection is refused, then held); with
  the daemon down, the container runs on its instance port, which the
  dashboard and `devstack status` show.
- The fixed port on every interface is bound by devstack instead of Docker:
  the same exposure as before, but a firewall rule for Docker's process no
  longer covers it, and Windows Firewall may ask once to allow devstack.
- One more forward per connection on the host (a copy in the daemon), as for
  dev servers.
- A UDP service (a game's voice port, DNS) does not follow a lane.
- Crosstalk stays possible where a stack makes it: a browser that calls an
  API by its fixed port while only the UI is named reaches the main tree's
  API from the lane's UI. Name both, or let the UI reach the API through its
  own origin (a dev proxy, a relative URL).
- The view of such a container shows the fixed and the instance port, as a
  proxied service's does.

## Enforced by

Where it will live:

- `crates/devstack-config/src/model.rs` (`main_ports` in `[compose.<group>]`,
  the check that it names services of the group, the UDP warning),
  `docs/stack-file.md`.
- `crates/devstack-compose/src/lib.rs` (the main tree's overlay for those
  services), `crates/devstack-daemon/src/supervisor/infra.rs` (the overlay in
  the main tree, the instance port from `compose ps`, the fronts).
- `crates/devstack-daemon/src/supervisor/handover.rs` (`moves_main_slot`),
  `crates/devstack-daemon/src/lanes.rs` (`take_main`, `give_back` flip the
  fronts).
- `crates/devstack-daemon/src/tcpfwd.rs` (`Front` on a given address, not
  loopback only).
- `docs/dashboard-api.md` (`InfraView`: fixed and instance port).

## What this ADR does not say

- Nothing about `[services.*]`: their main ports work as before.
- Nothing about a lane's own ports: they stay the Compose file's plus the
  offset.
- Nothing about naming in `devstack status` (#184 point 3): its container
  rows have no `SERVICE` header; `devstack top` does, which is a separate
  wording question.
