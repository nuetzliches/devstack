# ADR-0056: A lane on a node relays its UDP ports, a tunnel per client, a datagram per frame

- **Status:** Proposed
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (point 4 extended to UDP here), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0048](0048-a-containers-main-port-can-move-with-the-main-slot.md) (point 6: the main slot moves TCP only), [ADR-0052](0052-a-restart-carries-the-lanes-over.md), [ADR-0053](0053-whoever-a-lane-is-shared-with-connects-to-it.md), [#192](https://github.com/nuetzliches/devstack/issues/192), [#219](https://github.com/nuetzliches/devstack/issues/219)

## Context

A lane on a node forwards its services' and containers' TCP ports to the
requester's loopback (ADR-0029 point 4). Since 0.19.0 (#192) a container
port published as UDP (`53/udp`, a game's voice port, a SIP proxy) runs on
the node, is not forwarded, and the lane's page and `devstack lane list` say
so. A stack that needs such a port has to stay local.

The tunnel has no multiplexing to put datagrams in. As built (ADR-0029,
*Paths as built*), every forwarded connection is its own request over the
pool's mutual TLS: an HTTP upgrade (`devstack-tcp`) that the node agent
splices to the lane daemon's `/v1/tunnel/{port}`, which connects to the
container on the lane's network. A new one costs 5 to 30 ms (its TLS
handshake), a kept one 0.5 ms per round trip. The agent passes an upgrade
on without reading its protocol.

## Decision

1. **A tunnel per session, a frame per datagram.** A UDP session is its
   own HTTP upgrade, `Upgrade: devstack-udp`, on the same
   `/v1/tunnel/{port}` of the lane daemon, through the node agent as a TCP
   tunnel goes. On it each datagram is a frame: its length as two bytes,
   big-endian, then the datagram. Nothing else travels; no session id is
   needed, as the tunnel is the session.
2. **The relay on the requester.** It binds each of the lane's UDP ports on
   loopback (IPv4, and IPv6 where there is one), at the lane's port, as for
   TCP. A session is keyed by the client's address (IP and port). The first
   datagram of a client opens its tunnel; up to 64 datagrams wait while it
   opens, more are dropped. A tunnel that cannot be opened (the container
   does not run yet, the node does not answer) drops what waited, ends the
   session, and the next datagram of that client tries again, at most once
   a second. There is no hold while a container starts: what arrives
   before it answers is lost, as on a network. At most 256 sessions per
   port; a further client's datagrams are dropped until one ends.
3. **The idle timeout is 60 s** without a datagram either way. Protocols
   that keep a session send keepalives well inside that (NAT keepalives,
   SIP, WireGuard, QUIC every 15 to 30 s); a one-off exchange (a DNS query
   from a fresh source port) holds a tunnel to the node for no more than a
   minute. Docker's own UDP proxy keeps 90 s; shorter here, because each
   session holds a TLS connection on the node, whose server takes 1024 at
   once.
4. **The relay on the node.** The lane daemon answers the upgrade with a
   UDP socket of its own for the session, connected to the container's
   address on the lane's network (`dns:53`); what comes back from there
   goes back as frames. The session ends with its tunnel, or after twice
   the idle timeout without traffic, a backstop for a requester gone
   without closing. A port-unreachable error (the container restarts) is
   not an end: that datagram is lost, the next one is sent.
5. **Size.** A frame carries up to 65,535 bytes and both relays read with
   a 64 KiB buffer, so every datagram the operating system delivers
   (65,507 bytes at most over IPv4) arrives whole and leaves as one
   datagram of the same size. The tunnel is a TCP stream with no MTU of
   its own; on each end the operating system fragments a datagram larger
   than its link (loopback, the lane's Docker bridge), as it would for a
   local lane. A stream that ends inside a frame ends the session.
6. **A lost tunnel** ends its session only: its datagrams in flight are
   lost, and the client's next datagram opens a new session, with a new
   socket on the node, so the container sees a new source port, as after
   a NAT rebinding. Nothing is buffered or replayed across it. A lease that
   is over, a lane that stops or moves away closes the listeners and every
   session with them, as for TCP.
7. **Compatibility is asked for, not assumed.** The requester asks for UDP
   when it starts the lane (`relay_udp` in the start request). A node that
   relays writes that into the lane daemon's file (`udp`), and answers with
   the lane's UDP ports (`infra_udp_ports`) beside its TCP ones; only then
   does the lane daemon stop listing UDP ports as `not_forwarded`. An older
   requester does not ask: the ports stay `not_forwarded`, as it shows them
   today. An older node does not answer with them: the requester relays
   none, and the lane's `not_forwarded` ports come with the reason
   (`not_forwarded_reason`: the node's devstack forwards TCP only, with its
   release). A lane carried over a restart (ADR-0052) keeps its UDP ports.

## Alternatives considered

- **One tunnel per port, sessions multiplexed by an id in each frame.** One
  handshake per port instead of per client, but it needs a session protocol
  inside (open, close, flow control), and a slow session stalls every other
  one behind it on the same stream. The handshake is paid once per client
  and session, which a development client does not notice.
- **Datagrams over UDP to the node** (DTLS, QUIC, WireGuard). No
  head-of-line blocking, but a second listener and a second trust path
  beside the pool's mutual TLS, a UDP port to open in every node's firewall,
  and a connection through restrictive networks that TCP gets through.
- **The one multiplexed connection ADR-0029 point 3 decided.** It would
  carry datagrams as one more stream type, but it is not built, and
  rebuilding every tunnel for UDP is out of proportion.
- **Holding datagrams until the container answers.** UDP clients retry by
  themselves; a held datagram arrives late and out of order with the
  retries, which is worse than none.

## Consequences

- A TCP stream carries the datagrams: one lost packet on the way to the
  node delays the datagrams behind it of that session (head-of-line
  blocking) instead of dropping one. For development traffic on a pool
  link that is the better trade; a real-time media stream over a lossy
  link can stutter where it would have skipped.
- A client's first datagram waits for a TLS handshake to the node (5 to
  30 ms); each session holds a connection on the node until its timeout.
- The container sees the lane daemon's address, one source port per
  session, never the client's.
- Each session counts as a tunnel in the telemetry (`devstack.tunnels`).

## Enforced by

Will live in:

- `crates/devstack-daemon/src/udprelay.rs`: the framing, the requester's
  relay (sessions, idle timeout, limits), the lane daemon's session.
- `crates/devstack-daemon/src/api.rs`: `/v1/tunnel/{port}` with
  `devstack-udp`. `crates/devstack-daemon/src/supervisor/infra.rs`
  (`infra_udp`), `crates/devstack-daemon/src/external.rs` (`udp`, what is
  `not_forwarded`).
- `crates/devstack-daemon/src/remote.rs`: the listeners, `relay_udp`, the
  reason, `Carried`.
- `crates/devstack-node/src/node.rs` (`Start.relay_udp`,
  `LaneState.infra_udp_ports`), `crates/devstack-node/src/lanerun.rs` (the
  lane daemon's file).
- `docs/pool.md`, `docs/dashboard-api.md`, the `devstack` skill, the
  dashboard's texts, `devstack lane list`.

## What this ADR does not say

- That the main slot moves UDP ports. ADR-0048 point 6 stays: a UDP port of
  a named service stays Docker's in the main tree. Moving it needs a UDP
  front on the fixed port, which is a decision of its own.
- That a lane shared with others (ADR-0035, ADR-0053) relays UDP to them.
  A lane shares `http` and `https` services only; there is no UDP port to
  connect to.
- That other protocols are forwarded. An SCTP port stays `not_forwarded`.
- Anything about local lanes: Docker publishes their UDP ports itself.
