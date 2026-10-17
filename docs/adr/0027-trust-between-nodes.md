# ADR-0027: Nodes and requesters trust each other by pinned keys, never by network location

- **Status:** Accepted (2026-10-16); points 1 and 2 replaced by [ADR-0031](0031-a-coordinator-places-lanes.md) (certificates from the coordinator instead of pinned pairs), the rest stands; point 5 amended 2026-10-17
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0019](0019-control-api-security.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md)

## Context

ADR-0019 secures the daemon API for one user on one machine and keeps it on
loopback. Remote lanes (ADR-0025) need a second surface: a node agent reachable
from other machines, possibly over the internet, that starts containers running
someone else's code with someone else's development secrets.

Participants are trusted people on trusted machines, typically connected
through a WireGuard-based mesh VPN (for example NetBird or Tailscale). A mesh
authenticates machines to the network, but it is configured outside devstack,
may be absent, and a node may be reachable by other means; devstack cannot rely
on it as its only check.

## Decision

1. **Every node and every requester has its own key pair** (Ed25519), created
   by `devstack-node init` and on first use of a remote feature by `devstack`;
   the private key is stored like the API token of ADR-0019 (user profile,
   readable only by the owner).
2. **Trust is pinned, both ways.** A node holds an allowlist of requester keys
   (`devstack-node trust add <key>`); a requester holds its peer list with each
   node's address and key fingerprint (`devstack peers add <address>
   <fingerprint>`). No trust on first use, no certificate authority.
3. **Every connection is mutual TLS** with those keys. A request from a key not
   on the allowlist is refused before anything else happens, whatever network
   it arrives from.
4. **The node owner sets quotas per requester** (memory, CPU, number of lanes)
   on top of the shared capacity of ADR-0026.
5. **The lane container is confined**: not privileged, no Docker socket, memory
   and CPU limits equal to its lease, its own network. The `devstackd` inside it
   keeps ADR-0019 for its own API; only the agent talks to it.
6. **The trust boundary is stated, not hidden.** A lane's source, including
   untracked environment files with development secrets, is copied to the node
   (ADR-0029). The owner of the node can read it. Placing a lane on a node means
   trusting its owner with that; the docs say so.

*Amended 2026-10-17.* Point 5 held for the lane container, not for the
lane's infrastructure, which the node's Docker engine runs from the
requester's Compose files. Both are confined now as
[ADR-0033](0033-a-node-decides-what-runs-on-it.md) amended says (points 7
to 9): no privilege, no host namespace, no mount outside the lane's mirror,
limits on every container; the lane container also drops every capability.
Point 6 gains a second direction: a node trusts the members it runs lanes
for with its kernel and its Docker engine (a container escape is not
contained), and only a member an admin granted is a node at all
([ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) amended).

## Alternatives considered

- **Rely on the mesh VPN alone.** No key management in devstack, but any
  machine in the mesh, or anyone reaching the port by another route, could
  start containers.
- **A team certificate authority.** Scales better to many people, but someone
  has to run it, and revocation becomes a project of its own. Pinned keys are
  enough for a team-sized farm; a CA can be added later without changing the
  connection.
- **SSH as transport and identity.** Familiar and already allowed by ADR-0019,
  but on Windows nodes an SSH server is an extra component, and multiplexing
  ports, events and sync over SSH channels is harder than over one TLS
  connection.

## Consequences

- Adding a person to the farm means exchanging keys with every node they will
  use. Acceptable at team size; a shared signed peer list is the next step if
  it is not.
- Secrets in a lane's environment files are on another machine. Teams with
  secrets that must not leave the developer's machine keep those lanes local.

## Enforced by

Will live in: `crates/devstack-node` (allowlist, quotas, mTLS listener, container
confinement), the remote client in `crates/devstack-daemon` (peer list,
pinning), the user documentation of remote lanes.
