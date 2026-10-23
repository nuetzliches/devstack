# ADR-0057: A port a container announces moves inside the container too, and Compose is told its number

- **Status:** Accepted (2026-10-23)
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [#222](https://github.com/nuetzliches/devstack/issues/222) (item 3), [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md), [ADR-0048](0048-a-containers-main-port-can-move-with-the-main-slot.md), [ADR-0056](0056-a-lane-on-a-node-relays-its-udp-ports-through-a-tunnel-per-session.md)

## Context

A lane's overlay moves the host side of every published port by the lane's
offset (ADR-0030 point 2): `7881:7881` becomes `7981:7881`. Some servers
tell their clients which port to use: a WebRTC media server puts the port
it listens on into its ICE candidates (`7881/tcp`, `7882/udp`), a broker
its advertised listeners, others have an "external port" setting. In the
main tree the Compose file publishes them 1:1, so the number is right. In
a lane the server still says `7881`, and a client on the host sends its
media to the **main tree's** server while signalling, on a moved port,
reaches the lane's. devstack cannot see what a server announces.

Two facts bound the design. Compose interpolation (`${VAR:-default}`) has
no arithmetic: an offset alone cannot become a port in a Compose file. And
the lane overlay replaces `ports` (`!override`) from the model it reads, so
a `ports` entry the user builds from a variable would be moved a second
time. On a node nothing is published (ADR-0025); the requester forwards
TCP and relays UDP on loopback at the Compose file's host port plus the
lane's offset, the number a local lane would use (`remote.rs` maps each to
itself, ADR-0029, ADR-0056), and the node reaches the container at
`<service>:<target>` on the lane's network.

## Decision

1. **Opt-in per port.** `[compose.<group>] announced_ports = ["media:7881",
   "media:7882/udp"]` names ports (TCP without a suffix) whose number the
   server tells its clients. Each must be published 1:1 on a fixed port in
   the Compose files (`7881:7881`): the number a client reaches is the
   number the server listens on, in every tree.
2. **devstack keeps them 1:1.** In a lane the overlay publishes such a
   port as `7981:7981`: container port moved with the host port, so the
   server listens on, and announces, the lane's number.
3. **Compose is told the number.** For each named port devstack sets
   `DEVSTACK_PORT_<SERVICE>_<PORT>` (service upper-cased, `-` as `_`; the
   port as the files have it; TCP and UDP of one number share it) in the
   environment of every `docker compose` call for the group, which
   overrides an `env_files` value. Its value is the port clients reach in
   this tree: the files' port in the main tree, plus the offset in a lane.
   The user's files pass it to the server's listen and announce settings
   (`environment:` or `command:`) with the files' port as default
   (`${DEVSTACK_PORT_MEDIA_7881:-7881}`), so they run without devstack
   (ADR-0015). `ports` stays literal: devstack owns the mapping. A
   lane's `shared` service is the main tree's container: its value there
   is the files' port, and the lane's overlay leaves it alone.
   **`exec` steps** of commands get the variables of every group as the
   tree the command runs in has them, so a `docker compose` from a
   command step (`docker compose -p {compose.app.project} up -d media`)
   keeps the moved port instead of recreating the container on the
   files' one.
4. **A lane on a node** gets the same values: the node's agent sets them
   for its Compose calls from the lane's offset, its view of the lane's
   ports uses the moved container port, and the requester forwards and
   relays on that same number. The address the server announces is the
   user's setting; `127.0.0.1` is right locally and through the forwards.
5. **The main slot leaves them alone.** A `main_ports` service's announced
   ports stay Docker's, 1:1, and `promote --lane` does not move them: a
   client learns them from the server whose signalling it used, so it
   reaches that tree's server; holding them would send an earlier
   session's media to another tree.
6. **devstack keeps its own names.** Placeholders, ADR-0014 rewriting and
   the dashboard key the port by the files' number (`{compose.app.media.7881.port}`
   is `7981` in lane 1). The moved number reaches the same address as
   well, so an environment built from the variable
   (`media:${DEVSTACK_PORT_MEDIA_7881:-7881}`, `media:7981` in lane 1) is
   rewritten too.
7. **`devstack check`.** Errors: an entry that is not `service:port[/udp]`
   (a port range among them: ranges stay out), names no service of
   `services`, or a port not published 1:1 on a fixed port (a range, a
   Docker-assigned port, `17881:7881`; checked where the model is read: at
   every start, by a node's agent, and by `devstack check --compose`); an
   entry named twice; two entries with one variable name. A lane's start
   refuses an announced port whose moved number another port of the same
   service already has inside. Warnings, from the group's files and env
   files as text: a `DEVSTACK_PORT_*` no entry defines (a typo, or the key
   missing: the lane would announce the main tree's port), one Compose
   reads without a default, and an entry whose variable no file uses (the
   lane's server would not listen on the published port); and a file with
   the key whose `requires` is below the release that brings it.

## Alternatives considered

- **`DEVSTACK_PORT_OFFSET` only.** Compose cannot add it to a port; it
  reaches only an entrypoint script that does, and the overlay still moves
  only the host side. Withheld, so no file builds on "files' port plus
  offset", a rule devstack does not promise.
- **A variable per published port, nothing moved.** Enough for a server
  with a separate announce setting, but one that announces its listen port
  needs the container port moved, and a `ports` entry from the variable is
  moved twice. And with a variable for every port, a file that forgot the
  key would announce the main tree's port in a lane without a warning.
- **Move the container port, no variable.** The server would announce its
  old port and listen on nothing.
- **The user's own variable names** (`announced_ports = { MEDIA_TCP =
  "media:7881" }`). Readable, but a name in `env_files` would be overridden
  silently; the `DEVSTACK_` prefix is devstack's.

## Consequences

- Inside a lane's network the announced port is the moved one: a container
  or health check addressing `media:7881` must use the variable too.
- `docker compose` run by hand without the variables recreates the
  container on the default port; an `exec` step has them.
- A group with `announced_ports` reads its model (`compose config`) at
  every start in the main tree too, for the 1:1 check.
- `DEVSTACK_PORT_*` is devstack's: a file that set such a name itself
  before gets devstack's value for a named port. Additive otherwise
  (ADR-0023): a file without the key loads and runs as before; one with it
  raises `requires` to the release that brings it.
- Port ranges cannot be announced: `devstack check` rejects them.

## Enforced by

`crates/devstack-config` (`announced_ports`, `AnnouncedPort::variable`,
`ComposeGroup::port_variables`; in `validate.rs` the errors and
`announced_warnings`), `crates/devstack-compose/src/lib.rs` (`Group::env`,
set on every call; `overlay` keeps announced ports 1:1; `main_overlay`
skips them; `check_announced`), `crates/devstack-daemon/src/supervisor/infra.rs`
(the values per tree, the 1:1 check at a start, `files_port` and
`publish`: the published ports keyed by the files' port),
`crates/devstack-daemon/src/commands.rs` (`exec_env`),
`crates/devstack-node/src/lanerun.rs` and `confine.rs` (the agent's
variables, its check, `initial_infra`), `crates/devstack/src/dispatch/stack.rs`
(`check --compose`), `docs/stack-file.md`, `docs/pool.md`,
`skills/devstack-config/SKILL.md`, and the integration test
`an_announced_port_moves_inside_a_lanes_container`.

## What this ADR does not say

- Which address a server announces, or anything about ports a server is
  told by another channel (a config file it reads itself is not
  interpolated by Compose).
- How port ranges (a TURN relay range) could be announced: they are not.
