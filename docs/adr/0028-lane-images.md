# ADR-0028: A remote lane runs in a stack-provided toolchain image; the node supplies devstack

- **Status:** Accepted (2026-10-16), amended 2026-10-16 (a Dockerfile the node builds); point 3 noted 2026-10-26 (what `prepare` makes is in a store, [ADR-0062](0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md)); addendum 2026-10-29 (a stack without a lane image is refused before the lease)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md)

## Context

The lane container of ADR-0025 has to build and run the stack's services: it
needs their toolchains (.NET SDK, Node, …) and, to start quickly, their
dependency caches (NuGet packages, `node_modules`). Installing these on every
lane start costs minutes; a cold first build in a fresh snapshot costs about
five times a warm one (ADR-0002).

Toolchains belong to the project and change with it. devstack's own version
belongs to the node and the requester and must match their protocol.

## Decision

1. **The stack names a lane image** in the stack file (a new, optional key, for
   example `[lanes] image = "<registry>/<name>:<tag>"`, next to `port_step`
   of ADR-0024). The image is an
   ordinary Linux image with the stack's toolchains; it does not contain
   devstack. A stack without the key cannot place lanes remotely.
2. **The node supplies `devstackd`**: the agent mounts its own static Linux
   build of `devstackd` read-only into the lane container and starts it. The
   project image never has to be rebuilt for a devstack update, and the version
   inside a lane always matches the agent's.
3. **Layers are ordered for reuse**, and the docs and `devstack init` recommend
   that order: toolchains; dependency caches keyed by the lockfiles; optionally
   a warm build state of the main branch (`obj/`, framework caches), which the
   lane's snapshot then starts from.
   *Noted 2026-10-26.* The lane's copy is a bind mount at `/src` and `HOME`
   is the slot's `home/`: nothing the image holds there shows in the copy.
   Dependency caches in the image help only where the image puts them
   outside `HOME` (`NPM_CONFIG_CACHE`, `NUGET_PACKAGES`). What `prepare`
   makes (`node_modules`) is kept in the node's store
   ([ADR-0062](0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md)), not in the image.
4. **The project builds and publishes the image** (its CI, its registry); nodes
   pull it. A node that already holds its layers is preferred by placement
   (ADR-0026).
5. The new key is additive: files without it load and behave as before
   (ADR-0023).
6. *Amended 2026-10-16.* Instead of an image, the stack may name a Dockerfile
   in its repository: `[lanes] dockerfile = "dev/lane.Dockerfile"`. The node
   builds it from the lane's mirror and keeps it, keyed by the Dockerfile's
   content, so a project needs neither a registry nor a CI job for its lane
   image. `image` stays for teams that publish one; with both, `image` wins.

## Alternatives considered

- **devstack installs toolchains itself** (a list of SDK versions in the stack
  file). One file less for the project, but devstack would become a package
  manager for every ecosystem.
- **A devstack base image the project derives from.** Couples every project
  image to a devstack version; an update of devstack would mean rebuilding
  every lane image.
- **Dev Container definitions (`devcontainer.json`)** as the image source.
  Reuses an existing format where a project has one; can be supported as a
  second way to name the image later, it does not change this decision.

## Consequences

- A project that wants remote lanes maintains a Dockerfile and publishes an
  image. `devstack init` can generate a first version.
- The static Linux `devstackd` must run on any common base image (musl, no
  dynamic dependencies).
- A stale warm-state layer only costs build time: the snapshot overwrites what
  changed, and the build id is still the hash of the inputs (ADR-0002).

## Addendum 2026-10-29: a stack without a lane image goes to no node

A lane whose stack file (its own, else the main tree's, ADR-0055) names
neither `[lanes] image` nor `dockerfile` cannot run on any node. That was
found out on the node, after the lease and the sync. Approved by the owner
on 2026-10-29, three points:

1. The requester refuses a start or a move of such a lane on a node before
   it asks for a lease, with the node's words ("the stack names no lane
   image ...").
2. Placement (`GET /hub/pool/placement`, `devstack pool placement`, the MCP
   `placement`) marks every node of the coordinator's order `eligible:
   false` with that reason: the requester knows the stack file, the
   coordinator does not.
3. The dashboard says so on the lane's start card instead of offering the
   pool: its pool buttons and nodes are disabled, and a prompt for a coding
   agent (`lane-image`) names an image fitting the stack's toolchains.

A file that does not load is not refused here: the start says why.

## Enforced by

Will live in: `crates/devstack-config` (`[lanes] image`, with a frozen compat
file), `crates/devstack-node` (pull, mount of `devstackd`), `devstack
init` (Dockerfile template), the release workflow (static Linux build).

*Paths as built, noted 2026-10-18.* `crates/devstack-config/src/model.rs`
(`[lanes] image`, `dockerfile`, `sync`; frozen file
`crates/devstack-config/tests/compat/v1-0.9.0-remote.toml`),
`crates/devstack-node/src/lanerun.rs` (`image`: pull, or build from the
mirror without following a link, offline unless the node's network is `lan`),
`crates/devstack-node/src/node.rs` (`linux_binary`: the agent's own static
build, a copy per version, mounted read-only; `DEVSTACK_LANE_BIN` names
another one for tests), `.github/workflows/release.yml`
(`x86_64-unknown-linux-musl`). Not built: a Dockerfile template in `devstack
init` and placement by present layers (point 4).

*The addendum of 2026-10-29.* `crates/devstack-daemon/src/lanes.rs`
(`not_in_pool`, `lane_not_in_pool`, checked in `with_spec` before the lease;
`not_in_pool` of `GET /v1/lanes`), `crates/devstack-node/src/coordinator.rs`
(`not_in_pool` on an order), `crates/devstack-hub/src/pool.rs` (`placement`),
`crates/devstack/src/client.rs` (`placement`), `crates/devstack-hub/src/prompts.rs`
(`LaneImage`), `ui/src/components/LaneStart.vue`.
