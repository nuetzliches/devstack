# ADR-0033: A node decides who runs lanes on it, and what a lane there may do

- **Status:** Accepted (2026-10-16), amended 2026-10-17 (what a lane's Compose files may ask; limits on every container; the node's own policy), amended 2026-10-20 (the node runs the model it checked; known keys only; a lane's projects end with it); noted 2026-10-21 (what holds while a lane's containers start under its running daemon, #164)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (point 4: the node admits), [ADR-0027](0027-trust-between-nodes.md), [ADR-0031](0031-a-coordinator-places-lanes.md), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md)

## Context

A lane on a node runs the requester's code there: its build scripts, its
package installs, its services. Today every approved member may run any
repository on every node that shares, and a node checks only that the lane
fits what it offers. The lane container has no Docker socket, runs as the
agent's user and sees only its mirror, its home and the devstack binary,
but it has the node's network: on a server in an office network, that is
the office network.

For a developer machine that shares, its owner decides by sharing or not.
A server shared by a team needs more: who may run lanes there, and what a
lane may reach and take. Restricting *which repositories* run is what one
asks for first, but the requester names the repository and sends its
files: a node can hold an honest member to a list, it cannot hold a
dishonest one to it. What a node can enforce is who calls it (the member's
certificate) and what the container it starts may do.

## Decision

1. **Tags.** Admins give members tags, free-form and flat (`team:backend`,
   `company:nuts`, `customer:acme`); groups are tags. Nodes get tags too
   (`site:office`, `gpu`). The role (admin, member) stays what administers
   the pool (ADR-0032); tags say who may run what where.
2. **Who:** each node says which members may start lanes on it, by tag
   (`allow = ["team:backend"]`) or by member; by default every approved
   member. A lease for a member it does not allow is refused when the node
   admits it (ADR-0026 point 4), and the coordinator does not place such a
   lane there. A request may ask for node tags (`--on pool:gpu`), and the
   coordinator places only on nodes that carry them.
3. **Which repositories:** a node may name the repositories it runs (by the
   name the stack file gives, `[stack] name`); others are refused the same
   way. This guards against mistakes, not against a member who lies about
   the name, and the dashboard says so where it is set.
4. **How much:** a lane's container gets hard limits, CPU and memory, from
   what its lease admitted (`--cpus`, `--memory`), with room above the
   expected need the node's owner sets; a lane that outgrows them is
   stopped by the kernel, not the node.
5. **Which network:** a node sets what its lane containers may reach
   beyond their own infrastructure: the internet and the local network
   (the default today), the internet only (no private address ranges), or
   nothing. The node enforces it on the lane's networks (firewall rules on
   the host, or an egress proxy).
6. **Where it is set:** on the node (its agent's configuration, for a
   server deployed as code) and, for admins, in the pool area of the
   dashboard; the node reports its policy to the coordinator, which
   shows it and places lanes accordingly. The node is the one that
   enforces it.

*Amended 2026-10-17.* A lane's infrastructure runs on the node's own
Docker engine, from Compose files the requester sends; a lane's own code
writes into its mirror, links included. Without more, a Compose file was
root on the node (`privileged`, `pid: host`, a bind mount of `/`), and a
link in the mirror let the agent write outside it. So, on top of points 2
to 5:

7. **What a lane may ask of the node.** The agent reads a group's model with
   `docker compose config` in a throwaway container that sees only the
   lane's mirror, and refuses the lane if a service is privileged, adds
   capabilities or devices, shares a namespace of the host (`pid`, `ipc`,
   `uts`, `cgroup`, `network_mode`), sets `userns_mode`, `cgroup_parent`,
   `security_opt`, `sysctls` or `volumes_from` a container, or mounts,
   builds from or reads an env file at a path that does not resolve beneath
   the mirror; or if the project uses an external volume, a volume with
   `driver_opts`, an external network the stack file does not own, a
   network driver other than `bridge`, or, on an `internet` or `none` node,
   addresses or IPv6 of its own. The message names each.
8. **Limits hold for every container of a lane**, its infrastructure
   included (memory and CPU as point 4, and a process limit), and the lane
   container drops every capability and cannot gain privileges. A lane is
   taken to need at least 0.25 CPU and 256 MiB, whatever its requester
   says; a node's caps hold for its lanes together.
9. **Networks:** `none` gets the same firewall rules as `internet` and no
   traffic out of the lane's bridges at all; a lane's networks have no
   IPv6; images (the lane's and its Compose builds) are built without a
   network unless the node's network is `lan`, and are named so that only
   the node can tell the name, per member.
10. **Point 6, on the node itself:** `policy.json` in the agent's state holds
    on top of what an admin sets (both must admit; the smaller limits and
    the stricter network hold). The agent asks the coordinator for the
    node's policy before it admits its first lease, and never assumes none.

*Amended 2026-10-20.* Point 7 checked a model, and Compose on the node then
read the lane's files again to run one: what the lane (a request to sync,
one of its containers that still ran, its own code writing a link into the
mirror) changed between the two, or after, ran unchecked; values Compose
fills in from its own environment came from the agent's; and keys Compose
learnt since (`use_api_socket`, `provider`) were not refused. So:

11. **The node runs the model it checked.** The agent writes the model
    `config` gave, after the check, beside the mirror where the lane cannot
    write, and that file and its overlay are all Compose on the node reads.
    In it: only the services the lane starts; each bind mount of the mirror
    a mount of one volume bound to the mirror, with the path as its
    `subpath` (Docker resolves a subpath beneath its volume and refuses a
    link out of it at every start of the container, where a bind mount
    follows one); each secret's and config's file a copy; a variable left
    without a value (`environment: [NAME]`, a build's `args`) removed. Docker
    runs with only what it needs of the agent's environment (`PATH`, `HOME`,
    `DOCKER_*`). Before the check the lane's containers stop, and the node
    takes no sync for the lane while it starts.
12. **Known keys only.** A service may use the keys the node knows to stay
    in the container or checks; any other is refused by name until it is
    known. On top of point 7: `use_api_socket`, `provider`,
    `oom_kill_disable`, a negative `oom_score_adj`, a logging driver other
    than `json-file`/`local`/`none`, a privileged lifecycle hook, device
    reservations other than GPUs (`nvidia`), a build's `tags` (point 9: the
    node names what it builds), `cache_to` and local `cache_from`, and
    secrets or configs from `environment`.
13. **A lane's projects end with it.** The agent records each Compose
    project it creates for a lane, and takes them down by Compose's labels
    when the lane stops, its lease ends or its slot is purged, whatever the
    lane's files say by then.

*Noted 2026-10-21 (#164), on point 11.* The lane's daemon now starts
before its containers do ([ADR-0025](0025-a-lane-can-run-on-another-node.md),
noted the same day): the lane runs, and may change its files, while the
node starts them. Point 11 holds: everything the check covers is used
before the lane container starts. The model and its overlay are written,
the images pulled and built and the containers created (`compose create`)
first; after the daemon answered, Compose starts what was created and
builds nothing (`up --no-build`: a build context is the lane's to change by
then). Bind mounts stay subpaths of the mirror's volume, resolved at every
start; secrets and configs are copies. "While it starts" in point 11 ends
when the daemon answers: from then on the node takes syncs again, which
change nothing Compose still reads. A stop, the lease's end or a new start
ends the containers' start before the lane's containers are taken down or
its files read again (point 13).

Images a node pulls (`[lanes] image`, a service's `image:`) are pulled with
the node's credentials and can be run by any lane there by name; only images
it builds are named per member (point 9). `pool.md` says so.

This confines what a lane asks for, not the engine it runs on: the lane's
containers still share the node's kernel and Docker engine with other
lanes. A Docker engine per lane (rootless, or a VM per node) is the
stronger step and is not taken here.

## Alternatives considered

- **Trust every approved member (today).** Simple, and enough for a pool of
  a few colleagues' machines; not for a server in a network others rely on.
- **Repositories by content (a signed commit, a hash of the stack file).**
  The requester syncs a worktree with uncommitted changes, so there is no
  fixed content to sign; checking the remote URL only moves the trust to a
  string the requester sends.
- **Fixed groups or more roles.** A role per kind of access (lane runner,
  node owner, ...) multiplies with every node; groups that are tags do not.
- **A sandbox runtime (gVisor, Kata) for lane containers.** Stronger
  isolation of the kernel, at a cost in speed and setup per node; it can
  come on top of points 3 and 4 for nodes that want it.

## Consequences

- An admin decides per node; a node without a policy behaves as today.
- Limits make a lane that needs more than expected fail visibly instead of
  slowing the node down for everyone.
- Restricting the network breaks lanes that need a host on the local
  network (an internal package feed); such a node names exceptions.

## Enforced by

`crates/devstack-node/src/policy.rs` (the policy and its checks),
`crates/devstack-node/src/node.rs` (admission by member, tag and repository;
memory limit and network per lane), `crates/devstack-node/src/lanerun.rs`
(`docker run --memory`, `internal` networks for `none`),
`crates/devstack-node/src/coordinator.rs` (placement and named leases respect
the policy), `crates/devstack-node/src/egress.rs` (`internet`: iptables
chains per lane, set from a helper container on the host's network before
the lane's containers start), `crates/devstack-node/tests/node.rs` (a refused
and an admitted lease), the pool area in `ui/`. Not yet: CPU limits per lane
and placing by node tags (`--on pool:gpu`).

Amendment 2026-10-17: `crates/devstack-node/src/confine.rs` (point 7, with
its fixtures), `crates/devstack-node/src/safefs.rs` (no link followed in a
mirror), `crates/devstack-compose/src/lib.rs` (`node_overlay`: limits, offline
builds, image names, no IPv6), `crates/devstack-node/src/lanerun.rs`,
`crates/devstack-node/src/egress.rs`, `crates/devstack-node/src/policy.rs`
(`floor_need`, `strictest`).

*Noted 2026-10-18.* The "Not yet" above is half out of date: CPU limits per
lane are built (`max_cpus` in a policy, `--max-cpus` on `devstack node
members policy`, `--cpus` on every container of the lane in `lanerun.rs`).
Placing by node tags (`--on pool:gpu`) is not. `egress.rs` is this ADR's
point 5, not [ADR-0022](0022-network-egress-sandbox.md), which is unbuilt.

Amendment 2026-10-20: `crates/devstack-node/src/confine.rs` (`check`,
`render`, `strip_unset`), `crates/devstack-node/src/lanerun.rs` (`compose/`
beside the mirror, `projects.json`, `take_down` by label),
`crates/devstack-compose/src/lib.rs` (`clean_env`, `Docker::clean`),
`crates/devstack-node/src/node.rs` (no sync while a lane starts),
`crates/devstack-node/tests/lanerun.rs` (start, stop, purge on a real
engine).

*Noted 2026-10-19.* Point 7 checks what the lane starts: the services
`[compose.<group>] services` names, what they need (`depends_on`, `links`,
`network_mode`/`volumes_from` of a service), and the top-level networks,
volumes, secrets and configs those use (`check_model` in `confine.rs`). The
agent brings up nothing else (`compose create|up <services>`), and the lane
container has no Docker access, so the rest of a Compose file never runs on
the node. A group that names no services runs, and is checked, whole.
