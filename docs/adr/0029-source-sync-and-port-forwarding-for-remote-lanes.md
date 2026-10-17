# ADR-0029: A remote lane works on a mirror of the requester's tree; ports and API come back over one connection

- **Status:** Accepted (2026-10-16); point 2 amended (2026-10-16)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** plan §3.2, §3.4, [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0011](0011-platform-windows-native-first.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0027](0027-trust-between-nodes.md), [ADR-0035](0035-lanes-can-be-shared-with-the-pool.md) (point 4 used there: forwards to others in the pool)

## Context

The developer edits in a local working tree, often several writers at once
(IDE, agents). A remote lane has to react to every save as a local lane does
(plan §3.2), without a commit or push in between. Dev servers and hot-reload
mode (plan §3.4, §3.4a) run from the source tree itself, not from a snapshot,
so the node needs a live copy of the tree, not only snapshots.

On the way back, the developer's browser, tests and tools address services on
`localhost`, and dev servers keep a WebSocket open for hot module replacement.

## Decision

1. **The node keeps a mirror** of the requester's working tree per lane. Inside
   the lane container the mirror is the lane's source root; the `devstackd`
   there watches and snapshots it like any worktree. Its pipeline does not know
   it is remote. The mirror is on the node's own file system, so file events
   inside the container are reliable (unlike bind mounts from NTFS, ADR-0011).
2. **The requester syncs by content.** Its watcher (the existing debounce)
   collects changed paths with the snapshot's rule (tracked plus untracked
   non-ignored files, so environment files go too; ADR-0027 states the trust
   this needs), hashes them, asks the node which blobs it lacks, uploads only
   those and applies the change set to the mirror atomically. A first sync to a
   node with a mirror of the same repository sends only the difference.
   *Amended 2026-10-16:* files git ignores that the lane needs all the same
   (a local TLS certificate the browser trusts) are named in `[lanes] sync`
   and go too, from the worktree or, where it has none, from the main tree.
3. **One mutually authenticated connection** (ADR-0027) per lane carries
   multiplexed streams: sync, the remote daemon's API and event stream, and
   forwarded TCP connections.
4. **Every service port of the lane is forwarded** to a loopback listener on
   the requester, at the lane's allocated port (the stack's port plus
   `[lanes] port_step` times the lane's number, as for a local lane, ADR-0024),
   bytes unchanged, WebSocket upgrades and TLS dev servers included, like the
   TCP forward ADR-0024 uses for dev servers.
   Dev servers, hot module replacement included, therefore run in the remote
   lane like the rest of it.
5. **The requester's daemon proxies the remote lane's API** into its own
   (ADR-0007), and maps paths in compiler messages and logs from the mirror back
   to the local tree, as it already maps the snapshot (plan §3.2).

*Measured with M3, 2026-10-16:* the requester listens on each service's
lane port and carries every connection as an HTTP upgrade (`devstack-tcp`)
through the node agent to the lane daemon's `/v1/tunnel/{port}`, which
connects to the service on its container's loopback; only the stack's service
ports, behind the API's token and the pool's mutual TLS. From a Windows
machine to a Linux node: 0.5 ms per request on a kept-alive connection, 5 to
30 ms for a new one (its TLS handshake to the node). `TCP_NODELAY` on every
hop matters: without it each kept-alive request waited about 40 ms for a
delayed ACK, the daemon's own service proxy included.

*With M3c:* the containers of a remote lane are forwarded too, each on the
port a local lane would publish it on (the Compose file's host port moved by
the lane's offset; a port Docker assigns stays unforwarded, as it has no fixed
number to keep). The node lists those ports in the infrastructure file, the
lane daemon shows and links them, and its tunnel connects them to the
container on the lane's network.

## Alternatives considered

- **Commit and push, the node builds from the branch.** Needs a commit for every
  save; does not see untracked files; slower by a round trip through the Git
  host.
- **A network file system** (SMB, SSHFS) mounting the local tree on the node.
  Every build reads thousands of files across the network, and change events do
  not cross the mount reliably.
- **An existing sync tool** (Mutagen, Syncthing, rsync). Mutagen fits best, but
  is a separate daemon with its own session model and lifecycle; devstack
  already computes the changed file set and would only need blob transfer.
  Can be revisited if the built-in sync turns out to be the hard part.
- **Run dev servers locally against a remote backend.** Keeps the heaviest
  process (often the frontend dev server) and its `node_modules` on the machine
  the lane was moved away from.

## Consequences

- Save-to-build latency grows by one round trip plus the upload of the changed
  files; on a mesh link this is small next to the debounce (default 2 s).
- The node holds a copy of the source, including untracked files, while the
  mirror exists; it is deleted with the lane unless kept as a warm mirror for
  placement (ADR-0026), and never kept after the requester's key is removed
  from the allowlist.
- Two watchers (requester and lane) debounce in series.

## Enforced by

Will live in: the sync client in `crates/devstack-daemon` and its TCP forward
(`tcpfwd.rs`) with a remote target, the
mirror and connection handling in `crates/devstack-node`, path mapping next to
today's snapshot mapping.

*Paths as built, noted 2026-10-18.* `crates/devstack-daemon/src/remote.rs`
(`files`: the snapshot's list plus `[lanes] sync`; `follow_sync`: the
worktree is watched and a change synced within two seconds, everything
compared every 30 s besides, every two seconds without a watcher;
`forward_port` and `tunnel`: point 4; `forward`: point 5),
`crates/devstack-daemon/src/api.rs` (`/v1/tunnel/{port}` in the lane daemon),
`crates/devstack-daemon/src/lib.rs` (`display_root`, path mapping),
`crates/devstack-node/src/node.rs` (mirror, blobs per member capped at 8
GiB and hashed again as they are copied, the lane's API and tunnels),
`crates/devstack-node/src/safefs.rs` (writes into the mirror never follow a
link). Point 3 is built as separate requests over the pool's mutual TLS
(one per sync step, API call and forwarded connection, an HTTP upgrade for
streams and tunnels), not as one multiplexed connection. Lanes shared with
others are forwarded the same way through the node
([ADR-0035](0035-lanes-can-be-shared-with-the-pool.md)).
